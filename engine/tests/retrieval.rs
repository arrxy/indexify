use search_engine::index::{PageTable, PassageStore, prepare_passages};
use search_engine::retrieval::{Bm25, QueryMode, SearchEngine};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        static NEXT_DIR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "retrieval-{}-{}-{sequence}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn execute(binary: &str, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if started.elapsed() > Duration::from_secs(20) {
            child.kill().unwrap();
            panic!("Command did not exit: {binary} {args:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn build(input: &Path, index: &Path) {
    success(execute(
        env!("CARGO_BIN_EXE_search-engine"),
        &[
            input.to_str().unwrap(),
            "--output",
            index.to_str().unwrap(),
            "--sort",
            "cpu",
            "--threads",
            "3",
            "--batch-bytes",
            "96",
            "--block-size",
            "2",
            "--merge-fan-in",
            "3",
        ],
        "",
    ));
}
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}
fn fixture(root: &TestDir) -> (PathBuf, PathBuf, Vec<(u32, String)>) {
    let input = root.0.join("input.tsv");
    let index = root.0.join("index");
    let mut docs = Vec::new();
    for i in 0..36_u32 {
        let mut text = String::new();
        if i % 7 != 0 {
            if i % 2 == 0 {
                text.push_str(&"rust ".repeat((i % 4 + 1) as usize));
            }
            if i % 3 == 0 {
                text.push_str(&"search ".repeat((i % 3 + 2) as usize));
            }
            if i % 5 == 0 {
                text.push_str("Café 東京 ");
            }
            text.push_str(&"filler ".repeat((i % 5 + 1) as usize));
        }
        docs.push((1000 + i * 13, text));
    }
    // Equal scores exercise deterministic doc-ID tie ordering at the heap cutoff.
    for i in 0..14 {
        docs.push((3000 + i, "rust search".into()));
    }
    let text: String = docs
        .iter()
        .map(|(id, text)| format!("{id}\t{text}\n"))
        .collect();
    fs::write(&input, text).unwrap();
    build(&input, &index);
    (input, index, docs)
}

#[test]
fn daat_and_or_top_ten_match_independent_exhaustive_bm25() {
    let root = TestDir::new();
    let (_input, index, docs) = fixture(&root);
    let mut engine = SearchEngine::open(&index, None, true, 1.2, 0.75).unwrap();
    let counts: Vec<HashMap<String, u32>> = docs
        .iter()
        .map(|(_, text)| {
            let mut counts = HashMap::new();
            for term in tokens(text) {
                *counts.entry(term).or_default() += 1;
            }
            counts
        })
        .collect();
    let lengths: Vec<u32> = counts.iter().map(|m| m.values().sum()).collect();
    let n = docs.len() as f64;
    let average = lengths.iter().map(|&x| x as f64).sum::<f64>() / n;
    for query in [
        "rust search",
        "Rust RUST, search",
        "café 東京",
        "search missingterm",
        "onlymissing",
        "...",
        "filler",
        "rust search café 東京",
    ] {
        let mut terms = Vec::new();
        for t in tokens(query) {
            if !terms.contains(&t) {
                terms.push(t);
            }
        }
        for mode in [QueryMode::Conjunctive, QueryMode::Disjunctive] {
            let mut expected = Vec::new();
            for (doc_id, counts) in counts.iter().enumerate() {
                let matched: Vec<bool> = terms.iter().map(|t| counts.contains_key(t)).collect();
                let include = !terms.is_empty()
                    && match mode {
                        QueryMode::Conjunctive => matched.iter().all(|&m| m),
                        QueryMode::Disjunctive => matched.iter().any(|&m| m),
                    };
                if !include {
                    continue;
                }
                let mut parts = Vec::new();
                for t in &terms {
                    let tf = *counts.get(t).unwrap_or(&0) as f64;
                    let df = counts_for_term(&docs, t) as f64;
                    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                    let score = idf * (tf * 2.2)
                        / (tf + 1.2 * (0.25 + 0.75 * lengths[doc_id] as f64 / average));
                    parts.push(score);
                }
                expected.push((doc_id as u32, parts.iter().sum::<f64>(), parts));
            }
            expected.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let result = engine.search(query, mode).unwrap();
            assert_eq!(result.terms, terms);
            assert_eq!(
                result.matched_documents as usize,
                expected.len(),
                "{query} {mode:?}"
            );
            assert_eq!(result.hits.len(), expected.len().min(10));
            for (hit, (doc_id, score, parts)) in result.hits.iter().zip(expected) {
                assert_eq!(hit.doc_id, doc_id, "{query} {mode:?}");
                assert_eq!(hit.passage_id, docs[doc_id as usize].0);
                assert!((hit.score - score).abs() < 1e-10);
                assert!(
                    (hit.term_scores.iter().map(|p| p.score).sum::<f64>() - hit.score).abs()
                        < 1e-10
                );
                for ((part, term), expected_score) in hit.term_scores.iter().zip(&terms).zip(parts)
                {
                    assert_eq!(&part.term, term);
                    assert_eq!(
                        part.frequency,
                        *counts[doc_id as usize].get(term).unwrap_or(&0)
                    );
                    assert!((part.score - expected_score).abs() < 1e-10);
                }
                assert!(hit.snippet.contains('['));
            }
        }
    }
}
fn counts_for_term(docs: &[(u32, String)], term: &str) -> usize {
    docs.iter()
        .filter(|(_, text)| tokens(text).iter().any(|t| t == term))
        .count()
}

#[test]
fn process_handles_queries_mode_changes_errors_and_eof_without_restart() {
    let root = TestDir::new();
    let input = root.0.join("input.tsv");
    let index = root.0.join("index");
    fs::write(&input, "80\trust search\n10\trust rust\n500\tsearch\n").unwrap();
    build(&input, &index);
    let output = success(execute(
        env!("CARGO_BIN_EXE_search"),
        &["--index", index.to_str().unwrap()],
        "rust search\n/mode and\nrust search\n/mode xor\nunknownzz\n/mode or\nrust search\n/help\n/quit\nignored\n",
    ));
    assert_eq!(output.matches("Query:").count(), 4);
    assert_eq!(output.matches("Loaded ").count(), 1);
    assert_eq!(output.matches("3 matching documents").count(), 2);
    assert!(output.contains("1 matching documents"));
    assert!(output.contains("0 matching documents"));
    assert!(output.contains("Mode must be 'and' or 'or'"));
    assert!(output.contains("terms: rust="));
    assert!(output.contains("snippet: [rust] [search]"));
    assert!(!output.contains("Query: ignored"));
    let output = success(execute(
        env!("CARGO_BIN_EXE_search-engine"),
        &[
            "search",
            "--index",
            index.to_str().unwrap(),
            "--no-snippets",
        ],
        "rust\n/and\nsearch\n/or\n",
    ));
    assert_eq!(output.matches("Query:").count(), 2);
    assert!(!output.contains("snippet:"));
}

#[test]
fn legacy_page_table_upgrade_preserves_fields_and_handles_byte_offsets() {
    let root = TestDir::new();
    let input = root.0.join("input.tsv");
    let original = "0007\tCafé\tmore\r\n9\t \r\n11\t東京 rust";
    fs::write(&input, original).unwrap();
    let page = root.0.join("page_table.bin");
    let legacy: Vec<u8> = [7_u32, 2, 9, 0, 11, 2]
        .iter()
        .flat_map(|n| n.to_le_bytes())
        .collect();
    fs::write(&page, &legacy).unwrap();
    let old = PageTable::load(&page).unwrap();
    assert!(old.source.is_none());
    assert_eq!(old.entries.len(), 3);
    assert_eq!(prepare_passages(&root.0, &input).unwrap(), 3);
    let table = PageTable::load(&page).unwrap();
    assert_eq!(
        table
            .entries
            .iter()
            .map(|e| (e.passage_id, e.doc_length))
            .collect::<Vec<_>>(),
        vec![(7, 2), (9, 0), (11, 2)]
    );
    let mut source = PassageStore::open(&table, None).unwrap();
    for (entry, expected) in table.entries.iter().zip(["Café\tmore", " ", "東京 rust"]) {
        assert_eq!(source.text(entry).unwrap(), expected);
    }
    assert_eq!(fs::read_to_string(input).unwrap(), original);
    let saved = fs::read(&page).unwrap();
    for bad in [
        "8\twrong\n9\t\n11\trust",
        "7\ttoo few\n",
        "7\t\n9\t\n11\t\n12\textra\n",
    ] {
        let bad_path = root.0.join("bad.tsv");
        fs::write(&bad_path, bad).unwrap();
        assert!(prepare_passages(&root.0, &bad_path).is_err());
        assert_eq!(fs::read(&page).unwrap(), saved);
    }
    assert_eq!(
        fs::read_dir(&root.0)
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".page_table-"))
            .count(),
        0
    );
    // Both old and new records must reject partial data.
    fs::write(&page, &legacy[..legacy.len() - 1]).unwrap();
    assert!(PageTable::load(&page).is_err());
    fs::write(&page, &saved[..saved.len() - 1]).unwrap();
    assert!(PageTable::load(&page).is_err());
}

#[test]
fn separate_build_merge_and_search_executables_produce_the_same_index() {
    let root = TestDir::new();
    let (input, combined, _) = fixture(&root);
    let runs = root.0.join("runs");
    let separate = root.0.join("separate");
    success(execute(
        env!("CARGO_BIN_EXE_build_runs"),
        &[
            input.to_str().unwrap(),
            "--output",
            runs.to_str().unwrap(),
            "--sort",
            "cpu",
            "--threads",
            "2",
            "--batch-bytes",
            "96",
        ],
        "",
    ));
    let original_runs: Vec<_> = fs::read_dir(&runs)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (p.clone(), fs::read(p).unwrap())
        })
        .collect();
    success(execute(
        env!("CARGO_BIN_EXE_merge_index"),
        &[
            runs.to_str().unwrap(),
            "--output",
            separate.to_str().unwrap(),
            "--block-size",
            "2",
            "--merge-fan-in",
            "3",
            "--merge-threads",
            "2",
        ],
        "",
    ));
    for name in [
        "postings.bin",
        "blocks.bin",
        "lexicon.bin",
        "page_table.bin",
        "metadata.bin",
    ] {
        assert_eq!(
            fs::read(combined.join(name)).unwrap(),
            fs::read(separate.join(name)).unwrap(),
            "{name}"
        );
    }
    assert_eq!(fs::read_dir(&separate).unwrap().count(), 5);
    for (p, bytes) in original_runs {
        assert_eq!(fs::read(p).unwrap(), bytes);
    }
    let output = success(execute(
        env!("CARGO_BIN_EXE_search"),
        &["--index", separate.to_str().unwrap()],
        "rust search\n/quit\n",
    ));
    assert!(output.contains("showing 10 results"));
}

#[test]
fn empty_and_zero_token_documents_remain_searchable_without_division_errors() {
    let root = TestDir::new();
    for (i, text, count) in [(0, "", 0), (1, "10\t...\n20\t \n", 2)] {
        let input = root.0.join(format!("empty{i}.tsv"));
        let index = root.0.join(format!("index{i}"));
        fs::write(&input, text).unwrap();
        build(&input, &index);
        let mut engine = SearchEngine::open(&index, None, true, 1.2, 0.75).unwrap();
        assert_eq!(engine.document_count(), count);
        for mode in [QueryMode::Conjunctive, QueryMode::Disjunctive] {
            assert!(engine.search("anything", mode).unwrap().hits.is_empty());
            assert!(engine.search("...", mode).unwrap().hits.is_empty());
        }
    }
}

#[test]
fn bm25_validates_parameters_and_known_term_contributions() {
    let stats = search_engine::index::CollectionStats {
        document_count: 4,
        total_doc_length: 20,
        tokenizer_id: 1,
        tokenizer_version: 1,
    };
    let bm = Bm25::new(&stats, 1.2, 0.75).unwrap();
    let idf = bm.idf(2).unwrap();
    assert!((idf - 2_f64.ln()).abs() < 1e-12);
    assert!((bm.score(3, 5, idf) - idf * 6.6 / 4.2).abs() < 1e-12);
    assert_eq!(bm.score(0, 5, idf), 0.0);
    assert_eq!(bm.idf(0).unwrap(), 0.0);
    assert!(bm.idf(5).is_err());
    for (k1, b) in [
        (f64::NAN, 0.75),
        (-1.0, 0.75),
        (1.2, 1.1),
        (1.2, -0.1),
        (1.2, f64::INFINITY),
    ] {
        assert!(Bm25::new(&stats, k1, b).is_err());
    }
}

#[test]
fn incompatible_metadata_and_missing_source_fail_with_actionable_errors() {
    let root = TestDir::new();
    let (input, index, _) = fixture(&root);
    let moved = root.0.join("moved.tsv");
    fs::rename(&input, &moved).unwrap();
    assert!(SearchEngine::open(&index, None, true, 1.2, 0.75).is_err());
    let mut override_source = SearchEngine::open(&index, Some(&moved), true, 1.2, 0.75).unwrap();
    assert!(
        !override_source
            .search("rust", QueryMode::Disjunctive)
            .unwrap()
            .hits[0]
            .snippet
            .is_empty()
    );
    assert!(SearchEngine::open(&index, None, false, 1.2, 0.75).is_ok());
    let metadata = index.join("metadata.bin");
    let mut bytes = fs::read(&metadata).unwrap();
    bytes[20..24].copy_from_slice(&99_u32.to_le_bytes());
    fs::write(&metadata, bytes).unwrap();
    assert!(SearchEngine::open(&index, None, false, 1.2, 0.75).is_err());
}
