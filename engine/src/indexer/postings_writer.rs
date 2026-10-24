/*
Consumes sorted postings incrementally and writes compressed blocks plus
lexicon entries. The final merge can call push() directly without writing
an uncompressed merged file. Only the current posting block is buffered.
All integer fields use little-endian byte order.
*/
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

use super::read_next_posting;
use crate::compression::IntegerCodec;

pub struct PostingsWriter<'a, C: IntegerCodec> {
    postings: BufWriter<File>,
    lexicon: BufWriter<File>,
    blocks: BufWriter<File>,
    blocks_position: u64,
    list_blocks_start: u64,
    block_count: u32,
    codec: &'a C,
    block_size: usize,
    position: u64,
    list_start: u64,
    current_term: Option<String>,
    previous_doc: Option<u32>,
    doc_freq: u32,
    term_count: u64,
    doc_ids: Vec<u32>,
    frequencies: Vec<u32>,
}

impl<'a, C: IntegerCodec> PostingsWriter<'a, C> {
    pub fn new(output_dir: &Path, block_size: usize, codec: &'a C) -> io::Result<Self> {
        let block_size_u32 = to_u32(block_size)?;
        if block_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Block size must be positive",
            ));
        }
        fs::create_dir_all(output_dir)?;
        let mut postings = BufWriter::new(File::create(output_dir.join("postings.bin"))?);
        let mut lexicon = BufWriter::new(File::create(output_dir.join("lexicon.bin"))?);
        let mut blocks = BufWriter::new(File::create(output_dir.join("blocks.bin"))?);
        postings.write_all(b"IDX1")?;
        postings.write_all(&[codec.id()])?;
        postings.write_all(&block_size_u32.to_le_bytes())?;
        lexicon.write_all(b"LEX2")?;
        blocks.write_all(b"BLK1")?;

        Ok(Self {
            postings,
            lexicon,
            blocks,
            blocks_position: 4,
            list_blocks_start: 4,
            block_count: 0,
            codec,
            block_size,
            position: 9,
            list_start: 9,
            current_term: None,
            previous_doc: None,
            doc_freq: 0,
            term_count: 0,
            doc_ids: Vec::with_capacity(block_size),
            frequencies: Vec::with_capacity(block_size),
        })
    }

    pub fn push(&mut self, term: &str, doc_id: u32, frequency: u32) -> io::Result<()> {
        if term.is_empty() || frequency == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Term must be nonempty and frequency positive",
            ));
        }
        if self
            .current_term
            .as_deref()
            .is_some_and(|current| term < current)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Terms are not sorted",
            ));
        }
        if self.current_term.as_deref() != Some(term) {
            self.finish_term()?;
            self.current_term = Some(term.to_owned());
            self.list_start = self.position;
            self.list_blocks_start = self.blocks_position;
            self.block_count = 0;
            self.previous_doc = None;
            self.doc_freq = 0;
        }
        if self.previous_doc.is_some_and(|previous| doc_id <= previous) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Doc IDs must strictly increase within a term",
            ));
        }
        self.previous_doc = Some(doc_id);
        self.doc_ids.push(doc_id);
        self.frequencies.push(frequency);
        self.doc_freq = self.doc_freq.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Document frequency exceeds u32")
        })?;
        if self.doc_ids.len() == self.block_size {
            self.flush_block()?;
        }
        Ok(())
    }

    // Must succeed before temporary merge inputs are deleted.
    pub fn finish(mut self) -> io::Result<u64> {
        self.finish_term()?;
        self.postings.flush()?;
        self.lexicon.flush()?;
        self.blocks.flush()?;
        Ok(self.term_count)
    }

    fn flush_block(&mut self) -> io::Result<()> {
        // write_block clears the vectors, so capture this metadata first.
        let Some(&last_doc_id) = self.doc_ids.last() else {
            return Ok(());
        };
        let block_offset = self.position;
        let written = write_block(
            &mut self.postings,
            &mut self.doc_ids,
            &mut self.frequencies,
            self.codec,
        )?;
        // Directory records are exactly 12 bytes, independent of Rust padding.
        self.blocks.write_all(&last_doc_id.to_le_bytes())?;
        self.blocks.write_all(&block_offset.to_le_bytes())?;
        self.position += written;
        self.blocks_position += 12;
        self.block_count += 1;
        Ok(())
    }

    fn finish_term(&mut self) -> io::Result<()> {
        if let Some(term) = self.current_term.take() {
            self.flush_block()?;
            write_term(
                &mut self.lexicon,
                &term,
                self.list_start,
                self.position - self.list_start,
                self.doc_freq,
                self.list_blocks_start,
                self.block_count,
            )?;
            self.term_count += 1;
        }
        Ok(())
    }
}

// Retained for callers that already have a merged binary file.
pub fn write_index<C: IntegerCodec>(
    merged_path: &Path,
    output_dir: &Path,
    block_size: usize,
    codec: &C,
) -> io::Result<u64> {
    let mut input = BufReader::new(File::open(merged_path)?);
    let mut writer = PostingsWriter::new(output_dir, block_size, codec)?;
    while let Some(posting) = read_next_posting(&mut input)? {
        writer.push(&posting.term, posting.doc_id, posting.term_frequency)?;
    }
    writer.finish()
}

fn write_block<C: IntegerCodec>(
    output: &mut impl Write,
    doc_ids: &mut Vec<u32>,
    frequencies: &mut Vec<u32>,
    codec: &C,
) -> io::Result<u64> {
    if doc_ids.is_empty() {
        return Ok(0);
    }

    let gaps: Vec<u32> = doc_ids.windows(2).map(|pair| pair[1] - pair[0]).collect();

    let gap_bytes = codec.encode(&gaps)?;
    let frequency_bytes = codec.encode(frequencies)?;

    // Five u32 fields: 20 bytes total.
    let header = [
        doc_ids[0],
        *doc_ids.last().unwrap(),
        to_u32(doc_ids.len())?,
        to_u32(gap_bytes.len())?,
        to_u32(frequency_bytes.len())?,
    ];

    for value in header {
        output.write_all(&value.to_le_bytes())?;
    }

    output.write_all(&gap_bytes)?;
    output.write_all(&frequency_bytes)?;

    let written = 20 + gap_bytes.len() as u64 + frequency_bytes.len() as u64;

    doc_ids.clear();
    frequencies.clear();

    Ok(written)
}

fn write_term(
    output: &mut impl Write,
    term: &str,
    offset: u64,
    length: u64,
    doc_freq: u32,
    blocks_offset: u64,
    block_count: u32,
) -> io::Result<()> {
    output.write_all(&to_u32(term.len())?.to_le_bytes())?;
    output.write_all(term.as_bytes())?;
    output.write_all(&offset.to_le_bytes())?;
    output.write_all(&length.to_le_bytes())?;
    output.write_all(&doc_freq.to_le_bytes())?;
    output.write_all(&blocks_offset.to_le_bytes())?;
    output.write_all(&block_count.to_le_bytes())
}

fn to_u32(value: usize) -> io::Result<u32> {
    u32::try_from(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Value exceeds u32"))
}

#[cfg(test)]
#[path = "../../tests/unit/indexer/postings_writer.rs"]
mod tests;
