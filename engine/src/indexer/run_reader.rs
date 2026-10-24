use super::builder::IntermediatePosting;
use std::io;
use std::io::{BufRead, Read};

fn read_u32(reader: &mut impl Read) -> std::io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

pub fn read_next_posting(reader: &mut impl BufRead) -> io::Result<Option<IntermediatePosting>> {
    if reader.fill_buf()?.is_empty() {
        return Ok(None);
    }
    let term_length = read_u32(reader)?;
    let mut term_bytes = vec![0; term_length as usize];
    reader.read_exact(&mut term_bytes)?;
    let term = String::from_utf8(term_bytes).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF-8 sequence in term")
    })?;
    let doc_id = read_u32(reader)?;
    let term_frequency = read_u32(reader)?;
    Ok(Some(IntermediatePosting {
        term,
        doc_id,
        term_frequency,
    }))
}
