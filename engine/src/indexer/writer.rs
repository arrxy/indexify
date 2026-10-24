/*
This module sorts intermediate postings by term, then by doc_id,
and writes each batch to a binary run file. CPU sorting reorders the
batch; GPU sorting returns indices that the writer follows directly.

Each record is stored as:
[term byte length: u32][UTF-8 term bytes][doc_id: u32][term_frequency: u32]

Integer fields use little-endian byte order. Writes are buffered
for efficient disk access.

These sorted run files are uncompressed. They will later be merged
to create the final compressed inverted index.
*/
use super::sorting::{PostingOrder, PostingSorter};
use crate::indexer::builder::IntermediatePosting;
use std::fs::File;
use std::io::{self, BufWriter, Write};
pub fn write_run(
    file_path: &str,
    postings: &mut [IntermediatePosting],
    sorter: &dyn PostingSorter,
) -> io::Result<()> {
    let order = sorter.sort(postings)?;
    #[cfg(any(feature = "gpu-sort", test))]
    if let PostingOrder::Indices(indices) = &order {
        if indices.len() != postings.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Posting order length does not match batch",
            ));
        }
    }
    let file = File::create(file_path)?;
    let mut writer = BufWriter::new(file);
    match order {
        PostingOrder::InPlace => {
            for posting in postings {
                write_posting(&mut writer, posting)?;
            }
        }
        #[cfg(any(feature = "gpu-sort", test))]
        PostingOrder::Indices(indices) => {
            for index in indices {
                let posting = postings.get(index as usize).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Posting index outside batch")
                })?;
                write_posting(&mut writer, posting)?;
            }
        }
    }
    writer.flush()
}

fn write_posting(writer: &mut impl Write, posting: &IntermediatePosting) -> io::Result<()> {
    let term_bytes = posting.term.as_bytes();
    let term_length = u32::try_from(term_bytes.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Term exceeds the maximum byte length",
        )
    })?;
    writer.write_all(&term_length.to_le_bytes())?;
    writer.write_all(term_bytes)?;
    writer.write_all(&posting.doc_id.to_le_bytes())?;
    writer.write_all(&posting.term_frequency.to_le_bytes())
}

#[cfg(test)]
#[path = "../../tests/unit/indexer/writer.rs"]
mod tests;
