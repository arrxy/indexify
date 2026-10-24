use super::*;
use crate::{compression::Simple9, index::Lexicon, indexer::PostingsWriter};
use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "directory-cursor-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    root
}

struct CountingCodec(Arc<AtomicUsize>);
impl IntegerCodec for CountingCodec {
    fn id(&self) -> u8 {
        1
    }
    fn encode(&self, values: &[u32]) -> io::Result<Vec<u8>> {
        Simple9.encode(values)
    }
    fn decode(&self, bytes: &[u8], count: usize) -> io::Result<Vec<u32>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Simple9.decode(bytes, count)
    }
}

#[test]
fn galloping_matches_linear_oracle_for_every_start_and_boundary() {
    for length in 0..100 {
        let blocks: Vec<_> = (0..length)
            .map(|i| BlockEntry {
                last_doc_id: i as u32 * 7 + 3,
                offset: i as u64 * 40,
            })
            .collect();
        for start in 0..=length + 1 {
            for target in (0..length as u32 * 7 + 10).chain([u32::MAX]) {
                let expected = (start..length).find(|&i| blocks[i].last_doc_id >= target);
                assert_eq!(
                    gallop(&blocks, start, target),
                    expected,
                    "length={length}, start={start}, target={target}"
                );
            }
        }
    }
}

#[test]
fn directory_records_and_galloping_cursor_match_written_postings() {
    let root = temp_dir();
    let codec = Simple9;
    let expected = [0, 2, 9, 10, 15, 100, 101, 200, 205, 1000];
    let mut writer = PostingsWriter::new(&root, 3, &codec).unwrap();
    for (i, &id) in expected.iter().enumerate() {
        writer.push("rust", id, i as u32 + 1).unwrap();
    }
    writer.push("z", 88, 12).unwrap();
    assert_eq!(writer.finish().unwrap(), 2);
    let lex = Lexicon::load(root.join("lexicon.bin")).unwrap();
    let entry = &lex.terms["rust"];
    assert_eq!(entry.blocks_offset, 4);
    assert_eq!(entry.block_count, 4);
    let mut directory = File::open(root.join("blocks.bin")).unwrap();
    let mut magic = [0; 4];
    directory.read_exact(&mut magic).unwrap();
    assert_eq!(&magic, b"BLK1");
    let mut postings = File::open(root.join("postings.bin")).unwrap();
    for (i, &expected_last) in [9, 100, 205, 1000].iter().enumerate() {
        let last = read_u32(&mut directory).unwrap();
        let offset = read_u64(&mut directory).unwrap();
        assert_eq!(last, expected_last);
        if i == 0 {
            assert_eq!(offset, entry.offset);
        }
        postings.seek(SeekFrom::Start(offset + 4)).unwrap();
        assert_eq!(read_u32(&mut postings).unwrap(), last);
    }
    assert_eq!(lex.terms["z"].blocks_offset, 4 + 4 * 12);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut r = PostingsReader::open(&root, entry, CountingCodec(calls.clone())).unwrap();
    assert_eq!(r.doc_freq(), 10);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(r.next_geq(201).unwrap(), Some(205));
    assert_eq!(r.frequency(), Some(9));
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(r.next_geq(0).unwrap(), Some(205));
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(r.next_geq(206).unwrap(), Some(1000));
    assert_eq!(r.frequency(), Some(10));
    assert_eq!(r.next_geq(1001).unwrap(), None);
    assert_eq!(r.frequency(), None);
    assert_eq!(r.next_geq(0).unwrap(), None);
    let mut a = PostingsReader::open(&root, entry, Simple9).unwrap();
    let mut b = PostingsReader::open(&root, &lex.terms["z"], Simple9).unwrap();
    assert_eq!(b.next_geq(0).unwrap(), Some(88));
    for (i, &id) in expected.iter().enumerate() {
        assert_eq!(a.next_geq(id).unwrap(), Some(id));
        assert_eq!(a.frequency(), Some(i as u32 + 1));
    }
    assert_eq!(a.next_geq(u32::MAX).unwrap(), None);
    assert_eq!(b.frequency(), Some(12));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_directory_and_wrong_block_headers_are_rejected() {
    let root = temp_dir();
    let codec = Simple9;
    let mut writer = PostingsWriter::new(&root, 2, &codec).unwrap();
    for id in [1, 3, 8] {
        writer.push("a", id, 1).unwrap();
    }
    writer.finish().unwrap();
    let lex = Lexicon::load(root.join("lexicon.bin")).unwrap();
    let entry = &lex.terms["a"];
    let bytes = fs::read(root.join("blocks.bin")).unwrap();
    fs::write(root.join("blocks.bin"), &bytes[..bytes.len() - 1]).unwrap();
    assert!(PostingsReader::open(&root, entry, Simple9).is_err());
    let mut bad = bytes.clone();
    bad[16..20].copy_from_slice(&0u32.to_le_bytes());
    fs::write(root.join("blocks.bin"), &bad).unwrap();
    assert!(PostingsReader::open(&root, entry, Simple9).is_err());
    fs::write(root.join("blocks.bin"), &bytes).unwrap();
    let mut data = fs::read(root.join("postings.bin")).unwrap();
    data[13..17].copy_from_slice(&999u32.to_le_bytes()); // First block's last ID.
    fs::write(root.join("postings.bin"), &data).unwrap();
    let mut r = PostingsReader::open(&root, entry, Simple9).unwrap();
    assert!(r.next_geq(0).is_err());
    fs::write(root.join("legacy.bin"), b"LEX1").unwrap();
    assert!(Lexicon::load(root.join("legacy.bin")).is_err());
    fs::remove_dir_all(root).unwrap();
}
