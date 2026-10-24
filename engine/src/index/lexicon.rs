use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

#[derive(Debug)]
pub struct Lexicon {
    pub terms: HashMap<String, LexiconEntry>,
}

#[derive(Debug)]
pub struct LexiconEntry {
    pub offset: u64,
    pub length: u64,
    pub doc_freq: u32,
    pub blocks_offset: u64,
    pub block_count: u32,
}

impl Lexicon {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        let file_size = file.metadata()?.len();
        let mut reader = BufReader::new(file);

        let mut magic = [0; 4];
        reader.read_exact(&mut magic)?;

        if &magic != b"LEX2" {
            return Err(invalid(
                "Expected LEX2 lexicon; rebuild older indexes to add block directories",
            ));
        }

        let mut terms = HashMap::new();
        let mut bytes_read = 4_u64;

        while !reader.fill_buf()?.is_empty() {
            let term_length = read_u32(&mut reader)?;
            bytes_read += 4;

            if term_length == 0 {
                return Err(invalid("Empty lexicon term"));
            }

            // Term + offset (8) + length (8) + doc_freq (4) + blocks_offset (8) + block_count (4).
            let remaining_entry = u64::from(term_length) + 32;

            if remaining_entry > file_size.saturating_sub(bytes_read) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Truncated lexicon entry",
                ));
            }

            let mut term_bytes = vec![0; term_length as usize];
            reader.read_exact(&mut term_bytes)?;

            let term = String::from_utf8(term_bytes)
                .map_err(|_| invalid("Invalid UTF-8 in lexicon term"))?;

            let entry = LexiconEntry {
                offset: read_u64(&mut reader)?,
                length: read_u64(&mut reader)?,
                doc_freq: read_u32(&mut reader)?,
                blocks_offset: read_u64(&mut reader)?,
                block_count: read_u32(&mut reader)?,
            };

            bytes_read += remaining_entry;

            if entry.length == 0 || entry.doc_freq == 0 {
                return Err(invalid("Empty posting list in lexicon"));
            }

            if entry.blocks_offset < 4
                || entry.block_count == 0
                || entry.block_count > entry.doc_freq
            {
                return Err(invalid("Invalid block-directory metadata"));
            }

            if terms.insert(term, entry).is_some() {
                return Err(invalid("Duplicate lexicon term"));
            }
        }

        Ok(Self { terms })
    }
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
