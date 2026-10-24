/* PTB2 stores source-file information and one 20-byte record per document:
passage_id:u32, doc_length:u32, passage_offset:u64, passage_length:u32 (LE).
Legacy 8-byte page tables remain readable and can be upgraded for snippets. */
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct PassageSource {
    pub path: PathBuf,
    pub file_size: u64,
    pub modified_secs: u64,
    pub modified_nanos: u32,
}

#[derive(Debug)]
pub struct PageTable {
    pub entries: Vec<PageTableEntry>,
    pub source: Option<PassageSource>,
}

#[derive(Debug, Clone, Copy)]
pub struct PageTableEntry {
    pub passage_id: u32,
    pub doc_length: u32,
    pub passage_offset: u64,
    pub passage_length: u32,
}

impl PageTable {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let mut magic = [0; 4];
        if size >= 4 {
            reader.read_exact(&mut magic)?;
        }
        let (count, source) = if &magic == b"PTB2" {
            let count = read_u64(&mut reader)?;
            let file_size = read_u64(&mut reader)?;
            let modified_secs = read_u64(&mut reader)?;
            let modified_nanos = read_u32(&mut reader)?;
            let path_length = read_u32(&mut reader)? as usize;
            let expected = count
                .checked_mul(20)
                .and_then(|n| n.checked_add(36 + path_length as u64));
            if path_length > 65536 || modified_nanos >= 1_000_000_000 || expected != Some(size) {
                return Err(invalid("Invalid PTB2 page-table length or header"));
            }
            let mut path_bytes = vec![0; path_length];
            reader.read_exact(&mut path_bytes)?;
            let path =
                String::from_utf8(path_bytes).map_err(|_| invalid("Invalid UTF-8 source path"))?;
            (
                count,
                Some(PassageSource {
                    path: PathBuf::from(path),
                    file_size,
                    modified_secs,
                    modified_nanos,
                }),
            )
        } else {
            if size % 8 != 0 {
                return Err(invalid("Truncated legacy page-table record"));
            }
            reader.seek(SeekFrom::Start(0))?;
            (size / 8, None)
        };
        let count = usize::try_from(count).map_err(|_| invalid("Page table is too large"))?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let passage_id = read_u32(&mut reader)?;
            let doc_length = read_u32(&mut reader)?;
            let (passage_offset, passage_length) = if let Some(source) = &source {
                let offset = read_u64(&mut reader)?;
                let length = read_u32(&mut reader)?;
                if offset
                    .checked_add(u64::from(length))
                    .is_none_or(|end| end > source.file_size)
                {
                    return Err(invalid("Passage text extends beyond the source file"));
                }
                (offset, length)
            } else {
                (0, 0)
            };
            entries.push(PageTableEntry {
                passage_id,
                doc_length,
                passage_offset,
                passage_length,
            });
        }
        Ok(Self { entries, source })
    }

    pub fn get(&self, doc_id: u32) -> Option<&PageTableEntry> {
        self.entries.get(doc_id as usize)
    }
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    reader.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut b = [0; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
