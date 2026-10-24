mod parallel;
pub mod sorting;
pub use parallel::build_runs_parallel;

mod builder;
mod postings_writer;
mod reader;
mod run_reader;
mod writer;

pub use postings_writer::{PostingsWriter, write_index};
pub(crate) use run_reader::read_next_posting;

use crate::tokenizer::Tokenizer;
use builder::IndexBuilder;
use std::fs::{self, File};
use std::io;
use std::io::{BufWriter, Write};
use std::path::Path;

pub fn build_runs<T: Tokenizer>(
    input_path: &str,
    output_path: &str,
    max_batch_bytes: usize,
    tokenizer: &T,
    sorter: &dyn sorting::PostingSorter,
) -> io::Result<usize> {
    if max_batch_bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "max_batch_bytes must be greater than 0",
        ));
    }
    fs::create_dir_all(output_path)?;
    let page_file = File::create(Path::new(output_path).join("page_table.bin"))?;
    let mut page_writer = BufWriter::new(page_file);
    let mut builder = IndexBuilder::new();
    let mut run_count = 0;
    let passages = reader::read_passages_from_file(input_path)?;
    for (number, passage) in passages.enumerate() {
        let (passage_id, text) = passage?;
        let doc_id = u32::try_from(number)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Too many documents"))?;
        let tokens = tokenizer.tokenize(&text);
        let doc_length = builder.add_document(doc_id, tokens);
        page_writer.write_all(&passage_id.to_le_bytes())?;
        page_writer.write_all(&doc_length.to_le_bytes())?;
        if builder.buffered_bytes >= max_batch_bytes {
            run_count += 1;
            let path = format!("{output_path}/run_{run_count:04}.bin");
            builder.flush_to_file(&path, sorter)?;
        }
    }
    if builder.postings_count() > 0 {
        run_count += 1;
        let path = format!("{output_path}/run_{run_count:04}.bin");
        builder.flush_to_file(&path, sorter)?;
    }
    page_writer.flush()?;
    Ok(run_count)
}

pub fn inspect_run(file_path: &str) -> std::io::Result<()> {
    let file = std::fs::File::open(file_path)?;
    let mut reader = std::io::BufReader::new(file);

    while let Some(posting) = run_reader::read_next_posting(&mut reader)? {
        println!(
            "{} → (doc_id: {}, frequency: {})",
            posting.term, posting.doc_id, posting.term_frequency,
        );
    }
    Ok(())
}
