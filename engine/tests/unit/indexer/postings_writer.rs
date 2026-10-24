use super::*;
use crate::{compression::Simple9, index::Lexicon};
use std::io::{Read, Seek, SeekFrom};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "stream-writer-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn finish_flushes_tail_blocks_and_lexicon_entries() {
    let root = temp_dir("decode");
    let codec = Simple9;
    let mut writer = PostingsWriter::new(&root, 2, &codec).unwrap();
    let expected = [
        ("rust", vec![(0, 1), (1, 2), (4, 2)]),
        ("search", vec![(0, 1), (1, 1), (2, 1), (3, 1), (4, 1)]),
    ];
    for (term, postings) in &expected {
        for &(doc_id, frequency) in postings {
            writer.push(term, doc_id, frequency).unwrap();
        }
    }
    assert_eq!(writer.finish().unwrap(), 2);
    let lexicon = Lexicon::load(root.join("lexicon.bin")).unwrap();
    let mut file = File::open(root.join("postings.bin")).unwrap();
    let mut end = 9;
    for (term, expected_postings) in expected {
        let entry = &lexicon.terms[term];
        assert_eq!(entry.offset, end);
        assert_eq!(entry.doc_freq as usize, expected_postings.len());
        file.seek(SeekFrom::Start(entry.offset)).unwrap();
        let mut actual = Vec::new();
        while file.stream_position().unwrap() < entry.offset + entry.length {
            let mut bytes = [0; 20];
            file.read_exact(&mut bytes).unwrap();
            let fields: Vec<_> = bytes
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            let count = fields[2] as usize;
            let mut gaps = vec![0; fields[3] as usize];
            let mut frequencies = vec![0; fields[4] as usize];
            file.read_exact(&mut gaps).unwrap();
            file.read_exact(&mut frequencies).unwrap();
            let gaps = codec.decode(&gaps, count - 1).unwrap();
            let frequencies = codec.decode(&frequencies, count).unwrap();
            let mut ids = vec![fields[0]];
            for gap in gaps {
                ids.push(ids.last().unwrap() + gap);
            }
            assert_eq!(*ids.last().unwrap(), fields[1]);
            actual.extend(ids.into_iter().zip(frequencies));
        }
        end = entry.offset + entry.length;
        assert_eq!(file.stream_position().unwrap(), end);
        assert_eq!(actual, expected_postings);
    }
    assert_eq!(end, file.metadata().unwrap().len());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn streamed_and_file_based_writer_match() {
    let root = temp_dir("compatibility");
    let input = root.join("merged.bin");
    let mut data = Vec::new();
    let records = [("rust", 0_u32, 1_u32), ("rust", 3, 2), ("search", 1, 1)];
    for (term, id, frequency) in records {
        data.extend_from_slice(&(term.len() as u32).to_le_bytes());
        data.extend_from_slice(term.as_bytes());
        data.extend_from_slice(&id.to_le_bytes());
        data.extend_from_slice(&frequency.to_le_bytes());
    }
    fs::write(&input, data).unwrap();
    let from_file = root.join("from_file");
    assert_eq!(write_index(&input, &from_file, 2, &Simple9).unwrap(), 2);
    let streamed = root.join("streamed");
    let mut writer = PostingsWriter::new(&streamed, 2, &Simple9).unwrap();
    for (term, id, frequency) in records {
        writer.push(term, id, frequency).unwrap();
    }
    assert_eq!(writer.finish().unwrap(), 2);
    for name in ["postings.bin", "lexicon.bin", "blocks.bin"] {
        assert_eq!(
            fs::read(from_file.join(name)).unwrap(),
            fs::read(streamed.join(name)).unwrap()
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_posting_order_and_zero_frequency_are_rejected() {
    let root = temp_dir("invalid");
    let cases = [
        vec![("z", 0, 1), ("a", 1, 1)],
        vec![("a", 1, 1), ("a", 1, 1)],
        vec![("a", 2, 1), ("a", 1, 1)],
        vec![("a", 0, 0)],
        vec![("", 0, 1)],
    ];
    for (number, records) in cases.into_iter().enumerate() {
        let mut writer = PostingsWriter::new(&root.join(number.to_string()), 2, &Simple9).unwrap();
        let result: io::Result<()> = records
            .into_iter()
            .try_for_each(|(term, id, frequency)| writer.push(term, id, frequency));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
    fs::remove_dir_all(root).unwrap();
}
