/*
This module turns tokenized passages into intermediate posting records
of the form (term, doc_id, term_frequency).

Each distinct term produces one record per document. term_frequency
counts how many times the term occurs in that document.

The builder buffers these records in memory so they can later be
sorted and written to disk.
*/
use std::collections::HashMap;

#[derive(Default, Debug, Clone)]
pub struct IntermediatePosting {
    pub term: String,
    pub doc_id: u32,
    pub term_frequency: u32,
}

#[derive(Default, Debug)]
pub struct IndexBuilder {
    postings: Vec<IntermediatePosting>,
    pub(crate) buffered_bytes: usize,
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_document(&mut self, doc_id: u32, tokens: Vec<String>) -> u32 {
        let doc_len = tokens.len() as u32;
        let mut term_counts: HashMap<String, u32> = HashMap::new();
        for term in tokens {
            *term_counts.entry(term).or_insert(0) += 1;
        }
        for (term, frequency) in term_counts {
            self.buffered_bytes += 12 + term.len();
            self.postings.push(IntermediatePosting {
                term,
                doc_id,
                term_frequency: frequency,
            });
        }
        doc_len
    }

    pub fn postings_count(&self) -> usize {
        self.postings.len()
    }

    pub fn flush_to_file(
        &mut self,
        file_path: &str,
        sorter: &dyn super::sorting::PostingSorter,
    ) -> std::io::Result<()> {
        crate::indexer::writer::write_run(file_path, &mut self.postings, sorter)?;
        self.postings.clear();
        self.buffered_bytes = 0;
        Ok(())
    }
}
