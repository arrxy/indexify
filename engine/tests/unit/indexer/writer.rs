use super::*;
use crate::indexer::sorting::CpuSorter;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

struct IndexedSorter(Vec<u32>);
impl PostingSorter for IndexedSorter {
    fn sort(&self, _: &mut [IntermediatePosting]) -> io::Result<PostingOrder> {
        Ok(PostingOrder::Indices(self.0.clone()))
    }
    fn description(&self) -> &str {
        "test indexed sorter"
    }
}

fn temp_dir() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "indexed-writer-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    root
}

fn snapshot(postings: &[IntermediatePosting]) -> Vec<(String, u32, u32)> {
    postings
        .iter()
        .map(|p| (p.term.clone(), p.doc_id, p.term_frequency))
        .collect()
}

#[test]
fn indexed_writer_matches_cpu_bytes_without_moving_postings() {
    let root = temp_dir();
    let mut postings = vec![
        IntermediatePosting {
            term: "é".into(),
            doc_id: 7,
            term_frequency: 1,
        },
        IntermediatePosting {
            term: "z".into(),
            doc_id: u32::MAX,
            term_frequency: 2,
        },
        IntermediatePosting {
            term: "é".into(),
            doc_id: 0,
            term_frequency: 3,
        },
        IntermediatePosting {
            term: "a".into(),
            doc_id: 8,
            term_frequency: 4,
        },
    ];
    let original = snapshot(&postings);
    let mut cpu = postings.clone();
    let indexed_path = root.join("indexed.bin");
    let cpu_path = root.join("cpu.bin");
    write_run(
        indexed_path.to_str().unwrap(),
        &mut postings,
        &IndexedSorter(vec![3, 1, 2, 0]),
    )
    .unwrap();
    write_run(cpu_path.to_str().unwrap(), &mut cpu, &CpuSorter).unwrap();
    assert_eq!(snapshot(&postings), original);
    assert_eq!(fs::read(indexed_path).unwrap(), fs::read(cpu_path).unwrap());
    let empty_path = root.join("empty.bin");
    write_run(
        empty_path.to_str().unwrap(),
        &mut [],
        &IndexedSorter(vec![]),
    )
    .unwrap();
    assert_eq!(fs::metadata(empty_path).unwrap().len(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_index_count_and_out_of_bounds_return_errors() {
    let root = temp_dir();
    let mut postings = vec![IntermediatePosting {
        term: "rust".into(),
        doc_id: 0,
        term_frequency: 1,
    }];
    for (label, indices) in [("short", vec![]), ("out-of-bounds", vec![1])] {
        let path = root.join(label);
        let result = write_run(
            path.to_str().unwrap(),
            &mut postings,
            &IndexedSorter(indices),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        if label == "short" {
            assert!(!path.exists());
        }
    }
    fs::remove_dir_all(root).unwrap();
}
