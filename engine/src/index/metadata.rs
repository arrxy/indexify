use crate::index::page_table::PageTable;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::path::Path;

pub struct CollectionStats {
    pub document_count: u64,
    pub total_doc_length: u64,
    pub tokenizer_id: u32,
    pub tokenizer_version: u32,
}

impl CollectionStats {
    pub fn from_page_table(table: &PageTable) -> Self {
        Self {
            document_count: table.entries.len() as u64,
            total_doc_length: table
                .entries
                .iter()
                .map(|entry| entry.doc_length as u64)
                .sum(),
            tokenizer_id: 1,
            tokenizer_version: 1,
        }
    }

    pub fn average_doc_length(&self) -> f64 {
        if self.document_count == 0 {
            0.0
        } else {
            self.total_doc_length as f64 / self.document_count as f64
        }
    }

    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let mut file = File::create(path)?;
        file.write_all(b"STA1")?;
        file.write_all(&self.document_count.to_le_bytes())?;
        file.write_all(&self.total_doc_length.to_le_bytes())?;
        file.write_all(&self.tokenizer_id.to_le_bytes())?;
        file.write_all(&self.tokenizer_version.to_le_bytes())
    }

    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = File::open(path)?;

        if file.metadata()?.len() != 28 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Collection metadata must contain exactly 28 bytes",
            ));
        }

        let mut bytes = [0_u8; 28];
        file.read_exact(&mut bytes)?;

        if &bytes[..4] != b"STA1" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported collection metadata format",
            ));
        }

        Ok(Self {
            document_count: u64::from_le_bytes(bytes[4..12].try_into().unwrap()),
            total_doc_length: u64::from_le_bytes(bytes[12..20].try_into().unwrap()),
            tokenizer_id: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            tokenizer_version: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        })
    }
}
