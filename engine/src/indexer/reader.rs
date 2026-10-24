/*
This module contains code that takes user input and
reads the text data and generates passage ids and streams them to the indexer
The dataset here is a tsv file with the following format:
passage_id \t passage_text
*/

use std::io::{self, BufRead, BufReader};

pub fn read_passages_from_file(
    file_path: &str,
) -> io::Result<impl Iterator<Item = io::Result<(u32, String)>>> {
    let file = std::fs::File::open(file_path)?;
    let reader = BufReader::new(file);
    Ok(reader.lines().map(|line| {
        let line = line?;
        let (id, text) = line
            .split_once('\t')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid line format"))?;
        let passage_id: u32 = id
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid passage id"))?;
        Ok((passage_id, text.to_owned()))
    }))
}
