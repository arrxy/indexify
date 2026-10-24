/*
Reads one term's persisted block directory, then gallops over its last doc IDs.
Only the selected posting block is decompressed. The cursor moves forward,
keeps one decoded block, and owns an independent postings file handle.
*/
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use super::lexicon::LexiconEntry;
use crate::compression::IntegerCodec;

#[derive(Debug)]
struct BlockEntry {
    last_doc_id: u32,
    offset: u64,
}

pub struct PostingsReader<C: IntegerCodec> {
    reader: BufReader<File>,
    codec: C,
    block_size: u32,
    doc_freq: u32,
    list_end: u64,
    blocks: Vec<BlockEntry>,
    next_block: usize,
    doc_ids: Vec<u32>,
    frequencies: Vec<u32>,
    position: usize,
}

impl<C: IntegerCodec> PostingsReader<C> {
    // index_dir contains postings.bin and blocks.bin.
    pub fn open(index_dir: impl AsRef<Path>, entry: &LexiconEntry, codec: C) -> io::Result<Self> {
        let index_dir = index_dir.as_ref();
        let file = File::open(index_dir.join("postings.bin"))?;
        let file_size = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let mut header = [0_u8; 9];
        reader.read_exact(&mut header)?;
        if &header[..4] != b"IDX1" || header[4] != codec.id() {
            return Err(invalid("Invalid postings header or codec"));
        }
        let block_size = u32::from_le_bytes(header[5..].try_into().unwrap());
        let list_end = entry
            .offset
            .checked_add(entry.length)
            .ok_or_else(|| invalid("Posting-list range overflows"))?;
        if block_size == 0
            || entry.doc_freq == 0
            || entry.length == 0
            || entry.offset < 9
            || list_end > file_size
        {
            return Err(invalid("Invalid posting-list metadata"));
        }
        // All blocks except the final block are full in this writer's format.
        if entry.block_count != entry.doc_freq.div_ceil(block_size) {
            return Err(invalid(
                "Block count does not match doc_freq and block size",
            ));
        }

        let file = File::open(index_dir.join("blocks.bin"))?;
        let directory_size = file.metadata()?.len();
        let mut directory = BufReader::new(file);
        let mut magic = [0_u8; 4];
        directory.read_exact(&mut magic)?;
        if &magic != b"BLK1" {
            return Err(invalid("Invalid block-directory header"));
        }
        let directory_end = entry
            .blocks_offset
            .checked_add(u64::from(entry.block_count) * 12)
            .ok_or_else(|| invalid("Block-directory range overflows"))?;
        if entry.blocks_offset < 4 || directory_end > directory_size {
            return Err(invalid("Block directory extends beyond blocks.bin"));
        }
        directory.seek(SeekFrom::Start(entry.blocks_offset))?;
        let mut blocks: Vec<BlockEntry> = Vec::with_capacity(entry.block_count as usize);
        for _ in 0..entry.block_count {
            let block = BlockEntry {
                last_doc_id: read_u32(&mut directory)?,
                offset: read_u64(&mut directory)?,
            };
            if block.offset < entry.offset || block.offset > list_end.saturating_sub(20) {
                return Err(invalid("Block offset outside posting list"));
            }
            if let Some(previous) = blocks.last() {
                if block.last_doc_id <= previous.last_doc_id || block.offset < previous.offset + 20
                {
                    return Err(invalid("Block directory is not strictly ordered"));
                }
            } else if block.offset != entry.offset {
                return Err(invalid("First block does not start at the list offset"));
            }
            blocks.push(block);
        }
        Ok(Self {
            reader,
            codec,
            block_size,
            doc_freq: entry.doc_freq,
            list_end,
            blocks,
            next_block: 0,
            doc_ids: Vec::new(),
            frequencies: Vec::new(),
            position: 0,
        })
    }

    pub fn doc_freq(&self) -> u32 {
        self.doc_freq
    }

    pub fn doc_id(&self) -> Option<u32> {
        self.doc_ids.get(self.position).copied()
    }

    pub fn frequency(&self) -> Option<u32> {
        self.frequencies.get(self.position).copied()
    }

    // Return the current document if it already satisfies target. Otherwise move forward; None means exhausted.
    pub fn next_geq(&mut self, target: u32) -> io::Result<Option<u32>> {
        self.position += self.doc_ids[self.position..].partition_point(|&doc_id| doc_id < target);
        if let Some(doc_id) = self.doc_id() {
            return Ok(Some(doc_id));
        }
        let Some(block_index) = gallop(&self.blocks, self.next_block, target) else {
            self.next_block = self.blocks.len();
            self.doc_ids.clear();
            self.frequencies.clear();
            self.position = 0;
            return Ok(None);
        };
        self.load_block(block_index)?;
        self.position = self.doc_ids.partition_point(|&doc_id| doc_id < target);
        Ok(self.doc_id())
    }

    fn load_block(&mut self, index: usize) -> io::Result<()> {
        let entry = &self.blocks[index];
        let block_end = self
            .blocks
            .get(index + 1)
            .map_or(self.list_end, |b| b.offset);
        self.reader.seek(SeekFrom::Start(entry.offset))?;
        let first = read_u32(&mut self.reader)?;
        let last = read_u32(&mut self.reader)?;
        let count = read_u32(&mut self.reader)?;
        let gap_bytes = read_u32(&mut self.reader)?;
        let frequency_bytes = read_u32(&mut self.reader)?;
        let consumed = index as u64 * u64::from(self.block_size);
        let expected_count =
            (u64::from(self.doc_freq) - consumed).min(u64::from(self.block_size)) as u32;
        if count != expected_count
            || first > last
            || last != entry.last_doc_id
            || (index > 0 && first <= self.blocks[index - 1].last_doc_id)
        {
            return Err(invalid(
                "Block header disagrees with directory or document count",
            ));
        }
        let encoded_end = entry
            .offset
            .checked_add(20 + u64::from(gap_bytes) + u64::from(frequency_bytes))
            .ok_or_else(|| invalid("Block range overflows"))?;
        if encoded_end != block_end {
            return Err(invalid(
                "Block payload lengths disagree with directory boundaries",
            ));
        }
        let mut gaps = vec![0_u8; gap_bytes as usize];
        let mut frequencies = vec![0_u8; frequency_bytes as usize];
        self.reader.read_exact(&mut gaps)?;
        self.reader.read_exact(&mut frequencies)?;
        let gaps = self.codec.decode(&gaps, count as usize - 1)?;
        let frequencies = self.codec.decode(&frequencies, count as usize)?;
        if gaps.len() != count as usize - 1
            || frequencies.len() != count as usize
            || frequencies.contains(&0)
        {
            return Err(invalid("Invalid decoded block sizes or frequencies"));
        }
        let mut doc_ids = Vec::with_capacity(count as usize);
        let mut doc_id = first;
        doc_ids.push(doc_id);
        for gap in gaps {
            if gap == 0 {
                return Err(invalid("Document gaps must be positive"));
            }
            doc_id = doc_id
                .checked_add(gap)
                .ok_or_else(|| invalid("Document ID overflows"))?;
            doc_ids.push(doc_id);
        }
        if doc_id != last {
            return Err(invalid("Decoded last document ID differs from header"));
        }
        self.doc_ids = doc_ids;
        self.frequencies = frequencies;
        self.position = 0;
        self.next_block = index + 1;
        Ok(())
    }
}

// Check start, start+1, start+2, start+4, ...; binary-search the bracket.
fn gallop(blocks: &[BlockEntry], start: usize, target: u32) -> Option<usize> {
    if start >= blocks.len() {
        return None;
    }
    let mut lower = start;
    let mut upper = start;
    let mut step = 1_usize;
    while blocks[upper].last_doc_id < target {
        lower = upper + 1;
        if lower == blocks.len() {
            return None;
        }
        upper = start.saturating_add(step).min(blocks.len() - 1);
        step = step.saturating_mul(2);
    }
    Some(lower + blocks[lower..=upper].partition_point(|b| b.last_doc_id < target))
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "../../tests/unit/index/postings_reader.rs"]
mod tests;
