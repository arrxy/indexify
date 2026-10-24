/*
Computes one query term's BM25 contribution to one document.
Compute IDF once per distinct query term, then reuse it while traversing
that term's posting list. Sum contributions to obtain a document's score.
*/
use crate::index::CollectionStats;
use std::io;

pub struct Bm25 {
    document_count: u64,
    average_doc_length: f64,
    k1: f64,
    b: f64,
}

impl Bm25 {
    pub fn new(stats: &CollectionStats, k1: f64, b: f64) -> io::Result<Self> {
        if !k1.is_finite() || k1 < 0.0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "k1 must be finite and nonnegative",
            ));
        }
        if !b.is_finite() || !(0.0..=1.0).contains(&b) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "b must be between 0 and 1",
            ));
        }
        if stats.document_count == 0 && stats.total_doc_length != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "An empty collection cannot have indexed tokens",
            ));
        }
        Ok(Self {
            document_count: stats.document_count,
            average_doc_length: stats.average_doc_length(),
            k1,
            b,
        })
    }

    pub fn idf(&self, doc_freq: u32) -> io::Result<f64> {
        if u64::from(doc_freq) > self.document_count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Document frequency exceeds document count",
            ));
        }
        if doc_freq == 0 {
            return Ok(0.0);
        }
        let n = self.document_count as f64;
        let df = f64::from(doc_freq);
        Ok(((n - df + 0.5) / (df + 0.5)).ln_1p())
    }

    pub fn score(&self, term_frequency: u32, doc_length: u32, idf: f64) -> f64 {
        if term_frequency == 0 || self.average_doc_length == 0.0 {
            return 0.0;
        }
        let tf = f64::from(term_frequency);
        let length_normalization =
            1.0 - self.b + self.b * f64::from(doc_length) / self.average_doc_length;
        idf * (tf * (self.k1 + 1.0)) / (tf + self.k1 * length_normalization)
    }
}
