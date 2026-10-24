/* Adapt search-engine postings to metal-radix's numeric key-pair API.
Batch-local term ranks preserve the ordering required by the run merger.
The crate owns GPU pipelines, shader execution and result validation. */

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Mutex;

use metal_radix::GpuSorter as RadixSorter;

use super::{IntermediatePosting, PostingOrder, PostingSorter};

pub struct GpuSorter {
    sorter: Mutex<RadixSorter>,
    description: String,
}

impl GpuSorter {
    pub fn new() -> io::Result<Self> {
        let sorter = RadixSorter::new()?;
        let description = sorter.description().to_owned();
        Ok(Self {
            sorter: Mutex::new(sorter),
            description,
        })
    }
}

impl PostingSorter for GpuSorter {
    fn sort(&self, postings: &mut [IntermediatePosting]) -> io::Result<PostingOrder> {
        if postings.is_empty() {
            return Ok(PostingOrder::Indices(Vec::new()));
        }
        let keys = numeric_keys(postings)?;
        let sorter = self
            .sorter
            .lock()
            .map_err(|_| io::Error::other("GPU sorter poisoned"))?;
        let indices = sorter.sort_indices(&keys)?;
        Ok(PostingOrder::Indices(indices))
    }

    fn description(&self) -> &str {
        &self.description
    }
}

fn numeric_keys(postings: &[IntermediatePosting]) -> io::Result<Vec<[u32; 2]>> {
    if postings.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GPU batch has more than u32::MAX postings",
        ));
    }
    let mut unique: HashSet<&str> = HashSet::new();
    for posting in postings {
        unique.insert(posting.term.as_str());
    }
    let mut vocabulary: Vec<_> = unique.into_iter().collect();
    vocabulary.sort_unstable();
    let ids: HashMap<_, _> = vocabulary
        .into_iter()
        .enumerate()
        .map(|(id, term)| (term, id as u32))
        .collect();
    Ok(postings
        .iter()
        .map(|posting| [ids[posting.term.as_str()], posting.doc_id])
        .collect())
}

#[cfg(test)]
#[path = "../../../tests/unit/indexer/sorting/gpu.rs"]
mod tests;
