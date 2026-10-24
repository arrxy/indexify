/* Add text offsets to a completed page table in one sequential TSV scan.
The original TSV remains the text store. Only the top-ranked passages are
read at query time; no passage text or positions are added to postings. */
use super::page_table::{PageTable, PageTableEntry};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn prepare_passages(
    index_dir: impl AsRef<Path>,
    dataset: impl AsRef<Path>,
) -> io::Result<usize> {
    let index_dir = index_dir.as_ref();
    let dataset = fs::canonicalize(dataset)?;
    let source_path = dataset
        .to_str()
        .ok_or_else(|| invalid("Dataset path must be UTF-8"))?;
    let source_file = File::open(&dataset)?;
    let metadata = source_file.metadata()?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("Dataset modification time predates Unix epoch"))?;
    let table_path = index_dir.join("page_table.bin");
    let table = PageTable::load(&table_path)?;
    let temporary = index_dir.join(format!(
        ".page_table-{}-{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let result = (|| -> io::Result<usize> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(b"PTB2")?;
        writer.write_all(&(table.entries.len() as u64).to_le_bytes())?;
        writer.write_all(&metadata.len().to_le_bytes())?;
        writer.write_all(&modified.as_secs().to_le_bytes())?;
        writer.write_all(&modified.subsec_nanos().to_le_bytes())?;
        let path_length =
            u32::try_from(source_path.len()).map_err(|_| invalid("Dataset path is too long"))?;
        if path_length > 65536 {
            return Err(invalid("Dataset path is too long"));
        }
        writer.write_all(&path_length.to_le_bytes())?;
        writer.write_all(source_path.as_bytes())?;
        let mut reader = BufReader::new(source_file);
        let mut line = String::new();
        let mut offset = 0_u64;
        for (doc_id, entry) in table.entries.iter().enumerate() {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                return Err(invalid("Dataset has fewer documents than the index"));
            }
            let contents = line.strip_suffix('\n').unwrap_or(&line);
            let contents = contents.strip_suffix('\r').unwrap_or(contents);
            let (id, _text) = contents
                .split_once('\t')
                .ok_or_else(|| invalid("Dataset row has no tab"))?;
            let passage_id = id
                .parse::<u32>()
                .map_err(|_| invalid("Invalid passage ID in dataset"))?;
            if passage_id != entry.passage_id {
                return Err(invalid(&format!(
                    "Dataset passage ID differs from index at document {doc_id}"
                )));
            }
            let passage_offset = offset;
            let passage_length = u32::try_from(contents.len())
                .map_err(|_| invalid("Passage text exceeds u32 length"))?;
            writer.write_all(&entry.passage_id.to_le_bytes())?;
            writer.write_all(&entry.doc_length.to_le_bytes())?;
            writer.write_all(&passage_offset.to_le_bytes())?;
            writer.write_all(&passage_length.to_le_bytes())?;
            offset += bytes as u64;
        }
        line.clear();
        if reader.read_line(&mut line)? != 0 {
            return Err(invalid("Dataset has more documents than the index"));
        }
        let after = reader.get_ref().metadata()?;
        if after.len() != metadata.len() || after.modified()? != metadata.modified()? {
            return Err(invalid("Dataset changed while building passage offsets"));
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&temporary, &table_path)?;
        Ok(table.entries.len())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub struct PassageStore {
    reader: BufReader<File>,
    file_size: u64,
}
impl PassageStore {
    pub fn open(table: &PageTable, dataset_override: Option<&Path>) -> io::Result<Self> {
        let source=table.source.as_ref().ok_or_else(||invalid("Page table has no passage offsets. Run prepare-snippets with the index and TSV first."))?;
        let path = dataset_override.unwrap_or(&source.path);
        let file = File::open(path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "Cannot open passage source {}: {e}. Use --dataset if it moved.",
                    path.display()
                ),
            )
        })?;
        let metadata = file.metadata()?;
        if metadata.len() != source.file_size {
            return Err(invalid(
                "Passage source size differs from the indexed dataset",
            ));
        }
        if dataset_override.is_none() {
            let modified = metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_err(|_| invalid("Invalid passage source time"))?;
            if modified.as_secs() != source.modified_secs
                || modified.subsec_nanos() != source.modified_nanos
            {
                return Err(invalid(
                    "Passage source changed; regenerate offsets with prepare-snippets",
                ));
            }
        }
        Ok(Self {
            reader: BufReader::new(file),
            file_size: metadata.len(),
        })
    }

    pub fn text(&mut self, entry: &PageTableEntry) -> io::Result<String> {
        if entry
            .passage_offset
            .checked_add(u64::from(entry.passage_length))
            .is_none_or(|end| end > self.file_size)
        {
            return Err(invalid("Passage outside source file"));
        }
        self.reader.seek(SeekFrom::Start(entry.passage_offset))?;
        let mut row = vec![0; entry.passage_length as usize];
        self.reader.read_exact(&mut row)?;
        let row = String::from_utf8(row).map_err(|_| invalid("Passage is not UTF-8"))?;
        let (id, text) = row
            .split_once('\t')
            .ok_or_else(|| invalid("Passage source row has no tab"))?;
        if id.parse::<u32>().ok() != Some(entry.passage_id) {
            return Err(invalid(
                "Passage source ID disagrees with page-table offset",
            ));
        }
        Ok(text.to_owned())
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
