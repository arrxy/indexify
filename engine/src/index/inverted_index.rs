use crate::index::lexicon::Lexicon;
use std::fs::File;

pub struct InvertedIndex {
    pub lexicon: Lexicon,
    pub postings_file: File,
}
struct DecodedBlock {
    doc_ids: Vec<u32>,
    frequencies: Vec<u32>,
}

struct BlockHeader {
    first_doc_id: u32,
    last_doc_id: u32,
    posting_count: u32,
    doc_gaps_bytes: u32,
    frequencies_bytes: u32,
}
