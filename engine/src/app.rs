use crate::{compression, index, indexer, merger, tokenizer};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

const HELP: &str = "Search: search-engine search --index <directory>
Snippets: search-engine prepare-snippets --index <directory> --dataset <dataset.tsv>

Indexing usage: search-engine <dataset.tsv> [options]

Options:
  --output <directory>      Output directory (default: ./output/index)
  --batch-bytes <number>    Serialized bytes per batch (default: 65536)
  --block-size <number>     Postings per compressed block (default: 128)
  --merge-fan-in <number>   Maximum inputs per merge (default: 64; maximum: 128)
  --threads <number>       Run-building workers (default: 1)
  --merge-threads <number> Merge workers (default: 1; capped by open-file budget)
  --sort <cpu|gpu>          Posting sort backend (default: gpu; CPU-only builds default to cpu)
  --keep-intermediate      Keep intermediate files (default: remove on success)
  --help                   Show this help
";

struct Options {
    dataset: String,
    output: PathBuf,
    batch_bytes: usize,
    block_size: usize,
    merge_fan_in: usize,
    keep_intermediate: bool,
    threads: usize,
    merge_threads: usize,
    sort: String,
}

pub fn run_index(args: impl Iterator<Item = String>) -> io::Result<()> {
    let Some(options) = parse_options(args)? else {
        return Ok(());
    };

    if !Path::new(&options.dataset).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Dataset file not found",
        ));
    }

    let total_start = Instant::now();
    let sort_start = Instant::now();
    let sorter = indexer::sorting::create_sorter(&options.sort)?;
    let sort_init = sort_start.elapsed();
    if let Some(parent) = options.output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::create_dir(&options.output).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            invalid("Output directory already exists; choose a new --output directory")
        } else {
            error
        }
    })?;

    let work = options.output.join("_work");
    fs::create_dir(&work)?;
    let work_str = work
        .to_str()
        .ok_or_else(|| invalid("Output path must be UTF-8"))?;
    println!(
        "Workers: build {}, merge {}; batch threshold {} bytes per build worker",
        options.threads, options.merge_threads, options.batch_bytes
    );
    println!(
        "Sort backend: {}; initialization {:.2}s",
        sorter.description(),
        sort_init.as_secs_f64()
    );

    let stage = Instant::now();
    let run_count = indexer::build_runs_parallel(
        &options.dataset,
        work_str,
        options.batch_bytes,
        &tokenizer::SimpleTokenizer,
        options.threads,
        sorter.as_ref(),
    )?;
    println!(
        "Build: {run_count} runs in {:.2}s",
        stage.elapsed().as_secs_f64()
    );
    let runs: Vec<PathBuf> = (1..=run_count)
        .map(|number| work.join(format!("run_{number:04}.bin")))
        .collect();

    let stage = Instant::now();
    let runs = merger::prepare_runs(
        runs,
        &work,
        options.merge_fan_in,
        options.merge_threads,
        options.keep_intermediate,
    )?;
    println!(
        "Intermediate merges: {} runs ready in {:.2}s",
        runs.len(),
        stage.elapsed().as_secs_f64()
    );

    let stage = Instant::now();
    let codec = compression::Simple9;
    let mut writer = indexer::PostingsWriter::new(&options.output, options.block_size, &codec)?;
    let posting_count = merger::merge_into(&runs, |term, doc_id, frequency| {
        writer.push(term, doc_id, frequency)
    })?;
    let term_count = writer.finish()?;
    fs::copy(
        work.join("page_table.bin"),
        options.output.join("page_table.bin"),
    )?;
    println!(
        "Final merge + compression: {posting_count} postings, {term_count} terms in {:.2}s",
        stage.elapsed().as_secs_f64()
    );
    let stage = Instant::now();
    let documents = index::prepare_passages(&options.output, &options.dataset)?;
    println!(
        "Passage offsets: {documents} documents in {:.2}s",
        stage.elapsed().as_secs_f64()
    );
    {
        let page_table = index::PageTable::load(options.output.join("page_table.bin"))?;

        let stats = index::CollectionStats::from_page_table(&page_table);

        stats.save(options.output.join("metadata.bin"))?;

        println!(
            "Metadata: {} documents, {} indexed tokens, average length {:.2}",
            stats.document_count,
            stats.total_doc_length,
            stats.average_doc_length(),
        );
    }

    // Only remove temporary files created inside this new output directory.
    if !options.keep_intermediate {
        fs::remove_dir_all(&work)?;
    }

    println!("Index: {}", options.output.display());
    println!("Total: {:.2}s", total_start.elapsed().as_secs_f64());
    Ok(())
}

fn parse_options(args: impl Iterator<Item = String>) -> io::Result<Option<Options>> {
    let mut args = args;
    let mut dataset = None; // To store the dataset path provided by the user
    let mut output = PathBuf::from("./output/index"); // Default output directory
    let mut batch_bytes = 64 * 1024 * 1024; // Batch bytes is the approximate-serialized size of postings the
    // builder buffers before writing them as a temporary sorted run. Default batch size in bytes (64 MB)
    let mut block_size = 128; // how many postings the index writer groups into one compressed block
    let mut merge_fan_in = 64; // Merge fan-in is the maximum number of sorted temporary run files the program merges together in one pass.
    let mut keep_intermediate = false; // creates a Boolean setting that controls whether temporary files are kept after indexing.
    let mut threads = 8; // Number of threads to use for indexing
    let mut merge_threads = 2; // Number of threads to use for merging
    let mut sort = String::from(if cfg!(feature = "gpu-sort") {
        "gpu"
    } else {
        "cpu"
    });

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--output" => output = PathBuf::from(next_value(&mut args, "--output")?),
            "--batch-bytes" => batch_bytes = positive_number(&mut args, "--batch-bytes")?,
            "--block-size" => block_size = positive_number(&mut args, "--block-size")?,
            "--merge-fan-in" => merge_fan_in = positive_number(&mut args, "--merge-fan-in")?,
            "--threads" => threads = positive_number(&mut args, "--threads")?,
            "--merge-threads" => merge_threads = positive_number(&mut args, "--merge-threads")?,
            "--sort" => sort = next_value(&mut args, "--sort")?,
            "--keep-intermediate" => keep_intermediate = true,
            _ if arg.starts_with('-') => return Err(invalid(format!("Unknown option: {arg}"))),
            _ => {
                if dataset.replace(arg).is_some() {
                    return Err(invalid("Provide exactly one dataset filename"));
                }
            }
        }
    }

    if sort != "cpu" && sort != "gpu" {
        return Err(invalid("--sort must be cpu or gpu"));
    }
    let dataset = dataset.ok_or_else(|| invalid(HELP))?;
    if !(2..=merger::MAX_OPEN_INPUTS).contains(&merge_fan_in) {
        return Err(invalid("--merge-fan-in must be between 2 and 128"));
    }
    if u32::try_from(block_size).is_err() {
        return Err(invalid("--block-size exceeds u32"));
    }

    Ok(Some(Options {
        dataset,
        output,
        batch_bytes,
        block_size,
        merge_fan_in,
        keep_intermediate,
        threads,
        merge_threads,
        sort,
    }))
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> io::Result<String> {
    args.next()
        .ok_or_else(|| invalid(format!("Missing value for {flag}")))
}

fn positive_number(args: &mut impl Iterator<Item = String>, flag: &str) -> io::Result<usize> {
    let value = next_value(args, flag)?;
    let number = value
        .parse::<usize>()
        .map_err(|_| invalid(format!("Invalid value for {flag}: {value}")))?;
    if number == 0 {
        return Err(invalid(format!("{flag} must be greater than zero")));
    }
    Ok(number)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub fn run_prepare(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut index_dir = None;
    let mut dataset = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--index" => index_dir = Some(PathBuf::from(next_value(&mut args, &arg)?)),
            "--dataset" => dataset = Some(PathBuf::from(next_value(&mut args, &arg)?)),
            "--help" | "-h" => {
                println!(
                    "Usage: search-engine prepare-snippets --index <directory> --dataset <dataset.tsv>"
                );
                return Ok(());
            }
            _ => return Err(invalid(format!("Unknown prepare-snippets option: {arg}"))),
        }
    }
    let index_dir = index_dir.ok_or_else(|| invalid("Missing --index"))?;
    let dataset = dataset.ok_or_else(|| invalid("Missing --dataset"))?;
    let start = Instant::now();
    let count = index::prepare_passages(&index_dir, dataset)?;
    println!(
        "Prepared passage offsets for {count} documents in {:.2}s",
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

// Stage 1 executable: all intermediate postings and the source manifest go
// to disk so stage 2 can run in a separate process.
pub fn run_build(args: impl Iterator<Item = String>) -> io::Result<()> {
    let args: Vec<_> = args.collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: build_runs <dataset.tsv> --output <new-runs-directory> [--threads N] [--batch-bytes N] [--sort cpu|gpu]"
        );
        return Ok(());
    }
    let Some(options) = parse_options(args.into_iter())? else {
        return Ok(());
    };
    let source = fs::canonicalize(&options.dataset)?;
    let sorter = indexer::sorting::create_sorter(&options.sort)?;
    fresh_directory(&options.output)?;
    let start = Instant::now();
    let runs = indexer::build_runs_parallel(
        &options.dataset,
        utf8(&options.output)?,
        options.batch_bytes,
        &tokenizer::SimpleTokenizer,
        options.threads,
        sorter.as_ref(),
    )?;
    use std::io::Write;
    let mut manifest = fs::File::create(options.output.join("run_manifest.bin"))?;
    let source = utf8(&source)?.as_bytes();
    manifest.write_all(b"RUN1")?;
    manifest.write_all(&(runs as u64).to_le_bytes())?;
    let length = u32::try_from(source.len()).map_err(|_| invalid("Source path too long"))?;
    manifest.write_all(&length.to_le_bytes())?;
    manifest.write_all(source)?;
    println!(
        "Wrote {runs} sorted runs in {:.2}s to {}",
        start.elapsed().as_secs_f64(),
        options.output.display()
    );
    Ok(())
}

// Stage 2 consumes the manifest, merges/compresses runs, then records source
// passage offsets. It preserves stage 1's files for inspection or another run.
pub fn run_merge(args: impl Iterator<Item = String>) -> io::Result<()> {
    let args: Vec<_> = args.collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: merge_index <runs-directory> --output <new-index-directory> [--merge-fan-in N] [--merge-threads N] [--block-size N] [--keep-intermediate]"
        );
        return Ok(());
    }
    let Some(options) = parse_options(args.into_iter())? else {
        return Ok(());
    };
    use std::io::Read;
    let input = Path::new(&options.dataset);
    let mut file = fs::File::open(input.join("run_manifest.bin"))?;
    let mut header = [0; 16];
    file.read_exact(&mut header)?;
    if &header[..4] != b"RUN1" {
        return Err(invalid("Invalid run manifest"));
    }
    let count = u64::from_le_bytes(header[4..12].try_into().unwrap());
    let path_length = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    if path_length > 65536 || file.metadata()?.len() != 16 + path_length as u64 {
        return Err(invalid("Invalid run manifest length"));
    }
    let mut source = vec![0; path_length];
    file.read_exact(&mut source)?;
    let source = String::from_utf8(source).map_err(|_| invalid("Source path is not UTF-8"))?;
    let count = usize::try_from(count).map_err(|_| invalid("Too many runs"))?;
    let paths: Vec<_> = (1..=count)
        .map(|n| input.join(format!("run_{n:04}.bin")))
        .collect();
    fresh_directory(&options.output)?;
    let work = options.output.join("_work");
    fs::create_dir(&work)?;
    let start = Instant::now();
    let paths = merger::prepare_runs(
        paths,
        &work,
        options.merge_fan_in,
        options.merge_threads,
        true,
    )?;
    let codec = compression::Simple9;
    let mut writer = indexer::PostingsWriter::new(&options.output, options.block_size, &codec)?;
    let postings = merger::merge_into(&paths, |term, doc, freq| writer.push(term, doc, freq))?;
    let terms = writer.finish()?;
    fs::copy(
        input.join("page_table.bin"),
        options.output.join("page_table.bin"),
    )?;
    index::prepare_passages(&options.output, source)?;
    let table = index::PageTable::load(options.output.join("page_table.bin"))?;
    index::CollectionStats::from_page_table(&table).save(options.output.join("metadata.bin"))?;
    if !options.keep_intermediate {
        fs::remove_dir_all(work)?;
    }
    println!(
        "Index: {}; {postings} postings, {terms} terms in {:.2}s",
        options.output.display(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

fn fresh_directory(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(path).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            invalid("Output directory exists; choose a new directory")
        } else {
            e
        }
    })
}
fn utf8(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| invalid("Path must be UTF-8"))
}
