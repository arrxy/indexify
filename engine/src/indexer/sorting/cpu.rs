/* Sort posting batches in place by term, then document ID. */
use std::io;

use super::{IntermediatePosting, PostingOrder, PostingSorter};

#[derive(Default)]
pub struct CpuSorter;

impl PostingSorter for CpuSorter {
    fn sort(&self, postings: &mut [IntermediatePosting]) -> io::Result<PostingOrder> {
        postings.sort_unstable_by(|a, b| a.term.cmp(&b.term).then_with(|| a.doc_id.cmp(&b.doc_id)));
        Ok(PostingOrder::InPlace)
    }

    fn description(&self) -> &str {
        "cpu"
    }
}
