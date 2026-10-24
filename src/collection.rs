use crate::{extract, model::*};
use anyhow::{Context, Result, bail, ensure};
use search_engine::{
    compression::Simple9,
    index, indexer, merger,
    retrieval::{QueryMode, SearchEngine},
    tokenizer::SimpleTokenizer,
};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

pub fn default_home() -> PathBuf {
    if let Some(root) = std::env::var_os("INDEXIFY_HOME") {
        return root.into();
    }
    if let Some(root) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(root).join("indexify");
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
        .join(".local/share/indexify")
}

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "Collection names must contain 1–80 letters, numbers, hyphens or underscores"
    );
    Ok(())
}

pub fn suggested_name(source: &Path) -> String {
    let stem = if source.is_dir() {
        source.file_name()
    } else {
        source.file_stem()
    };
    let name: String = stem
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(60)
        .collect();
    let name = name.trim_matches('-');
    if name.is_empty() {
        "documents".into()
    } else {
        name.into()
    }
}

fn generation(home: &Path, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    let directory = home.join(name);
    let current = fs::read_to_string(directory.join("CURRENT")).with_context(|| {
        format!("Collection '{name}' not found. Index a document or directory first")
    })?;
    let current = current.trim();
    ensure!(
        current.starts_with("gen-")
            && current
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "Invalid collection generation"
    );
    Ok(directory.join(current))
}

pub fn manifest(home: &Path, name: &str) -> Result<Manifest> {
    let value: Manifest =
        serde_json::from_reader(File::open(generation(home, name)?.join("manifest.json"))?)?;
    ensure!(
        value.version == 1,
        "Unsupported Indexify collection version"
    );
    Ok(value)
}

pub fn list(home: &Path) -> Result<Vec<Manifest>> {
    if !home.exists() {
        return Ok(vec![]);
    }
    let mut collections = Vec::new();
    for item in fs::read_dir(home)? {
        let item = item?;
        if item.file_type()?.is_dir() {
            let name = item.file_name().to_string_lossy().into_owned();
            if item.path().join("CURRENT").is_file() {
                collections.push(
                    manifest(home, &name)
                        .with_context(|| format!("Cannot load collection '{name}'"))?,
                );
            }
        }
    }
    collections.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(collections)
}

fn lock(home: &Path, name: &str, shared: bool) -> Result<File> {
    validate_name(name)?;
    fs::create_dir_all(home.join(name))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join(name).join(".lock"))?;
    if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    }
    .context(
        "Collection is in use. Close its other search sessions before refreshing or removing it",
    )?;
    Ok(file)
}

pub fn build(
    home: &Path,
    name: &str,
    source: &Path,
    replace: bool,
    progress: &mut dyn FnMut(Progress) -> Result<()>,
) -> Result<Manifest> {
    validate_name(name)?;
    let source =
        fs::canonicalize(source).with_context(|| format!("Cannot access {}", source.display()))?;
    ensure!(
        source.is_file() || source.is_dir(),
        "Choose a document or directory"
    );
    if source.is_file() {
        ensure!(
            extract::format(&source).is_some(),
            "Supported formats: PDF, MD, DOCX"
        );
    }
    let _lock = lock(home, name, false)?;
    let collection = home.join(name);
    ensure!(
        replace || !collection.join("CURRENT").exists(),
        "Collection '{name}' already exists. Use 'indexify refresh {name}' or choose another --name"
    );
    let stage = tempfile::Builder::new()
        .prefix(".building-")
        .tempdir_in(&collection)?;
    progress(Progress {
        stage: "Discovering documents".into(),
        detail: source.display().to_string(),
        done: 0,
        total: 0,
    })?;
    let mut files = Vec::new();
    let mut skipped = 0;
    let mut issues = Vec::new();
    if source.is_file() {
        files.push(source.clone());
    } else {
        for entry in WalkDir::new(&source)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        {
            match entry {
                Ok(e) if e.file_type().is_file() => {
                    if extract::format(e.path()).is_some()
                        && !e.file_name().to_string_lossy().starts_with("~$")
                    {
                        files.push(e.into_path());
                    } else {
                        skipped += 1;
                    }
                }
                Ok(_) => {}
                Err(error) => issues.push(ImportIssue {
                    source: error.path().unwrap_or(&source).to_owned(),
                    message: error.to_string(),
                }),
            }
        }
    }
    ensure!(!files.is_empty(), "No PDF, MD or DOCX documents found");
    let tsv_path = stage.path().join("passages.tsv");
    // Metadata is addressed by a compact offset array; passage text stays on disk.
    let mut tsv = BufWriter::new(File::create(&tsv_path)?);
    let mut metadata = BufWriter::new(File::create(stage.path().join("passages.jsonl"))?);
    let mut offsets = BufWriter::new(File::create(stage.path().join("offsets.bin"))?);
    let mut next_id = 0_u32;
    let mut metadata_offset = 0_u64;
    let mut successful = 0;
    for (number, path) in files.iter().enumerate() {
        progress(Progress {
            stage: "Extracting passages".into(),
            detail: path.display().to_string(),
            done: number,
            total: files.len(),
        })?;
        let format = extract::format(path).unwrap();
        // Roll back a partially extracted document if any page/parser fails.
        tsv.flush()?;
        metadata.flush()?;
        offsets.flush()?;
        let checkpoints = (
            tsv.stream_position()?,
            metadata.stream_position()?,
            offsets.stream_position()?,
            next_id,
        );
        let result = extract::extract(path, &mut |draft| {
            let id = next_id;
            if id.is_multiple_of(256) {
                progress(Progress {
                    stage: "Extracting passages".into(),
                    detail: path.display().to_string(),
                    done: number,
                    total: files.len(),
                })?;
            }
            next_id = next_id
                .checked_add(1)
                .context("Collection exceeds the engine's passage-ID limit")?;
            let item = PassageMeta {
                id,
                source: path.to_owned(),
                format: format.into(),
                location: draft.location,
                heading: safe_text(&draft.heading),
                page: draft.page,
                line: draft.line,
            };
            offsets.write_all(&metadata_offset.to_le_bytes())?;
            let record = serde_json::to_vec(&item)?;
            metadata.write_all(&record)?;
            metadata.write_all(b"\n")?;
            metadata_offset += record.len() as u64 + 1;
            writeln!(tsv, "{id}\t{}", draft.text)?;
            Ok(())
        });
        match result {
            Ok(count) if count > 0 => successful += 1,
            outcome => {
                tsv.flush()?;
                metadata.flush()?;
                offsets.flush()?;
                tsv.get_mut().set_len(checkpoints.0)?;
                tsv.seek(SeekFrom::Start(checkpoints.0))?;
                metadata.get_mut().set_len(checkpoints.1)?;
                metadata.seek(SeekFrom::Start(checkpoints.1))?;
                offsets.get_mut().set_len(checkpoints.2)?;
                offsets.seek(SeekFrom::Start(checkpoints.2))?;
                next_id = checkpoints.3;
                metadata_offset = checkpoints.1;
                issues.push(ImportIssue {
                    source: path.to_owned(),
                    message: match outcome {
                        Err(error) => format!("{error:#}"),
                        _ => "No searchable text found".into(),
                    },
                });
            }
        }
    }
    tsv.flush()?;
    metadata.flush()?;
    offsets.flush()?;
    drop(tsv);
    drop(metadata);
    drop(offsets);
    ensure!(
        successful > 0,
        "No documents could be indexed. {}",
        issues
            .iter()
            .map(|i| format!("{}: {}", i.source.display(), i.message))
            .collect::<Vec<_>>()
            .join("; ")
    );
    progress(Progress {
        stage: "Building search index".into(),
        detail: format!("{successful} documents · {next_id} passages"),
        done: files.len(),
        total: files.len(),
    })?;
    let work = stage.path().join("work");
    let index_dir = stage.path().join("index");
    fs::create_dir(&index_dir)?;
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let run_count = indexer::build_runs_parallel(
        tsv_path
            .to_str()
            .context("Collection storage path must be UTF-8")?,
        work.to_str()
            .context("Collection storage path must be UTF-8")?,
        16 * 1024 * 1024,
        &SimpleTokenizer,
        threads,
        &indexer::sorting::CpuSorter,
    )?;
    let runs = (1..=run_count)
        .map(|n| work.join(format!("run_{n:04}.bin")))
        .collect();
    let runs = merger::prepare_runs(runs, &work, 64, 2, false)?;
    progress(Progress {
        stage: "Compressing index".into(),
        detail: "Writing ranked-search data".into(),
        done: files.len(),
        total: files.len(),
    })?;
    let mut writer = indexer::PostingsWriter::new(&index_dir, 128, &Simple9)?;
    merger::merge_into(&runs, |term, doc_id, frequency| {
        writer.push(term, doc_id, frequency)
    })?;
    writer.finish()?;
    fs::copy(
        work.join("page_table.bin"),
        index_dir.join("page_table.bin"),
    )?;
    index::prepare_passages(&index_dir, &tsv_path)?;
    let table = index::PageTable::load(index_dir.join("page_table.bin"))?;
    index::CollectionStats::from_page_table(&table).save(index_dir.join("metadata.bin"))?;
    fs::remove_dir_all(&work)?;
    let index_bytes = WalkDir::new(stage.path())
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum();
    let manifest = Manifest {
        version: 1,
        name: name.into(),
        source,
        built_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        files: successful,
        passages: next_id as usize,
        skipped,
        issues,
        index_bytes,
    };
    let mut output = File::create(stage.path().join("manifest.json"))?;
    serde_json::to_writer_pretty(&mut output, &manifest)?;
    output.sync_all()?;
    progress(Progress {
        stage: "Saving collection".into(),
        detail: "Publishing the new snapshot".into(),
        done: files.len(),
        total: files.len(),
    })?;
    let generation_name = format!(
        "gen-{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let destination = collection.join(&generation_name);
    fs::rename(stage.path(), &destination)?;
    let mut pointer = tempfile::NamedTempFile::new_in(&collection)?;
    writeln!(pointer, "{generation_name}")?;
    pointer.as_file().sync_all()?;
    pointer.persist(collection.join("CURRENT"))?;
    // Exclusive collection lock guarantees no active reader uses old generations.
    for entry in fs::read_dir(&collection)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry.file_name().to_string_lossy().starts_with("gen-")
            && entry.path() != destination
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    Ok(manifest)
}

pub fn refresh(
    home: &Path,
    name: &str,
    progress: &mut dyn FnMut(Progress) -> Result<()>,
) -> Result<Manifest> {
    let previous = manifest(home, name)?;
    build(home, name, &previous.source, true, progress)
}

pub fn remove(home: &Path, name: &str) -> Result<()> {
    let _ = manifest(home, name)?;
    let _lock = lock(home, name, false)?;
    fs::remove_dir_all(home.join(name))?;
    Ok(())
}

pub struct Collection {
    pub manifest: Manifest,
    engine: SearchEngine,
    metadata: BufReader<File>,
    offsets: Vec<u64>,
    _lock: File,
}

impl Collection {
    pub fn open(home: &Path, name: &str) -> Result<Self> {
        let _lock = lock(home, name, true)?;
        let directory = generation(home, name)?;
        let manifest = manifest(home, name)?;
        let engine = SearchEngine::open(
            directory.join("index"),
            Some(&directory.join("passages.tsv")),
            true,
            1.2,
            0.75,
        )?;
        let bytes = fs::read(directory.join("offsets.bin"))?;
        ensure!(
            bytes.len() % 8 == 0 && bytes.len() / 8 == manifest.passages,
            "Passage catalog is inconsistent"
        );
        let offsets = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| u64::from_le_bytes(*b))
            .collect();
        let metadata = BufReader::new(File::open(directory.join("passages.jsonl"))?);
        Ok(Self {
            manifest,
            engine,
            metadata,
            offsets,
            _lock,
        })
    }

    pub fn search(&mut self, query: &str, mode: QueryMode) -> Result<Found> {
        let start = Instant::now();
        let result = self.engine.search(query, mode)?;
        let mut hits = Vec::new();
        for (index, hit) in result.hits.into_iter().enumerate() {
            let offset = self
                .offsets
                .get(hit.passage_id as usize)
                .context("Passage metadata missing")?;
            self.metadata.seek(SeekFrom::Start(*offset))?;
            let mut line = String::new();
            self.metadata.read_line(&mut line)?;
            let passage: PassageMeta = serde_json::from_str(&line)?;
            ensure!(passage.id == hit.passage_id, "Passage metadata ID mismatch");
            hits.push(Hit {
                rank: index + 1,
                score: hit.score,
                passage,
                text: self.engine.passage_text(hit.doc_id)?,
            });
        }
        Ok(Found {
            query: query.into(),
            mode: mode.label().into(),
            matched_passages: result.matched_documents,
            elapsed_ms: start.elapsed().as_millis(),
            hits,
        })
    }
}

pub fn safe_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn open_hit(hit: &Hit) -> Result<()> {
    ensure!(
        hit.passage.source.exists(),
        "Original document has moved or was removed. The indexed preview is still available"
    );
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg(&hit.passage.source)
            .status()?;
        ensure!(status.success(), "Could not open document");
    }
    #[cfg(target_os = "windows")]
    {
        // Explorer opens a document without passing its path through a shell.
        std::process::Command::new("explorer.exe")
            .arg(&hit.passage.source)
            .spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let status = std::process::Command::new("xdg-open")
            .arg(&hit.passage.source)
            .status()?;
        ensure!(status.success(), "Could not open document");
    }
    Ok(())
}

pub fn copy_text(text: &str) -> Result<()> {
    use std::process::{Command, Stdio};
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip.exe", &[])]
    } else {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])]
    };
    for (program, args) in candidates {
        if let Ok(mut child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            child
                .stdin
                .take()
                .context("Clipboard input unavailable")?
                .write_all(text.as_bytes())?;
            if child.wait()?.success() {
                return Ok(());
            }
        }
    }
    bail!("Clipboard unavailable. Install wl-copy or xclip on Linux, or use CLI search output")
}
