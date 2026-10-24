use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TestDir(PathBuf);
impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "parallel-index-{name}-{}-{}",
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

fn run(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_search-engine"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if started.elapsed() > Duration::from_secs(15) {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "Indexer did not terminate: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn build(input: &Path, output: &Path, threads: &str, merge_threads: &str) {
    let result = run(&[
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sort",
        "cpu",
        "--batch-bytes",
        "512",
        "--block-size",
        "2",
        "--merge-fan-in",
        "3",
        "--threads",
        threads,
        "--merge-threads",
        merge_threads,
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.join("_work").exists());
}

#[test]
fn parallel_build_and_merge_match_serial_bytes_and_page_order() {
    let root = TestDir::new("equivalence");
    let input = root.0.join("input.tsv");
    let mut text = String::new();
    let mut expected_pages = Vec::new();
    for i in 0_u32..640 {
        let passage_id = i * 11 + 7;
        let repetitions = if i % 17 == 0 { 20_u32 } else { 1 };
        let body = if i % 31 == 0 {
            String::new()
        } else {
            "Rust rust! Café naïve 123. ".repeat(repetitions as usize)
        };
        text.push_str(&format!("{passage_id}\t{body}\n"));
        expected_pages.extend_from_slice(&passage_id.to_le_bytes());
        expected_pages.extend_from_slice(
            &(if body.is_empty() {
                0_u32
            } else {
                5 * repetitions
            })
            .to_le_bytes(),
        );
    }
    fs::write(&input, text).unwrap();
    let serial = root.0.join("serial");
    build(&input, &serial, "1", "1");
    let table = search_engine::index::PageTable::load(serial.join("page_table.bin")).unwrap();
    let mut actual_pages = Vec::new();
    for entry in &table.entries {
        actual_pages.extend_from_slice(&entry.passage_id.to_le_bytes());
        actual_pages.extend_from_slice(&entry.doc_length.to_le_bytes());
    }
    assert_eq!(actual_pages, expected_pages);
    assert!(table.source.is_some());
    for (threads, merge_threads) in [("4", "1"), ("1", "3"), ("4", "3")] {
        let parallel = root.0.join(format!("parallel-{threads}-{merge_threads}"));
        build(&input, &parallel, threads, merge_threads);
        for name in [
            "postings.bin",
            "lexicon.bin",
            "page_table.bin",
            "blocks.bin",
        ] {
            assert_eq!(
                fs::read(serial.join(name)).unwrap(),
                fs::read(parallel.join(name)).unwrap(),
                "{name}: build workers {threads}, merge workers {merge_threads}"
            );
        }
    }
}

#[test]
fn empty_collections_and_documents_without_tokens_match_serial() {
    let root = TestDir::new("empty");
    for (name, contents) in [("empty", ""), ("no_tokens", "8\t...\n99\t   \n")] {
        let input = root.0.join(format!("{name}.tsv"));
        fs::write(&input, contents).unwrap();
        let serial = root.0.join(format!("{name}-serial"));
        let parallel = root.0.join(format!("{name}-parallel"));
        build(&input, &serial, "1", "1");
        build(&input, &parallel, "4", "2");
        for file in [
            "postings.bin",
            "lexicon.bin",
            "page_table.bin",
            "blocks.bin",
        ] {
            assert_eq!(
                fs::read(serial.join(file)).unwrap(),
                fs::read(parallel.join(file)).unwrap()
            );
        }
        assert_eq!(
            fs::metadata(parallel.join("postings.bin")).unwrap().len(),
            9
        );
        assert_eq!(fs::read(parallel.join("lexicon.bin")).unwrap(), b"LEX2");
        assert_eq!(fs::read(parallel.join("blocks.bin")).unwrap(), b"BLK1");
    }
}

#[test]
fn input_errors_and_invalid_worker_options_terminate_with_errors() {
    let root = TestDir::new("errors");
    let input = root.0.join("bad.tsv");
    let mut text = (0..2000)
        .map(|i| format!("{i}\trust search\n"))
        .collect::<String>();
    text.push_str("missing tab\n");
    fs::write(&input, text).unwrap();
    let output = root.0.join("invalid-input");
    let result = run(&[
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sort",
        "cpu",
        "--threads",
        "4",
        "--merge-threads",
        "2",
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Invalid line format"));
    assert!(!output.join("postings.bin").exists());
    for (flag, value) in [
        ("--threads", "0"),
        ("--merge-threads", "0"),
        ("--merge-fan-in", "129"),
    ] {
        let invalid_output = root.0.join(flag.trim_start_matches('-'));
        let result = run(&[
            input.to_str().unwrap(),
            "--output",
            invalid_output.to_str().unwrap(),
            flag,
            value,
        ]);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains(flag));
        assert!(!invalid_output.exists());
    }
    let help = run(&["--help"]);
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--threads") && help.contains("--merge-threads"));
}

#[test]
fn final_merge_streams_without_creating_an_uncompressed_final_run() {
    let root = TestDir::new("fused-final-merge");
    let input = root.0.join("input.tsv");
    fs::write(&input, "100\tRust makes search fast.\n205\tRust rust powers search.\n310\tSearch finds documents, documents.\n415\tCafé search supports naïve users.\n520\tRUST, search! Rust?\n").unwrap();
    let mut outputs = Vec::new();
    for fan_in in ["128", "2"] {
        let output = root.0.join(format!("fan-in-{fan_in}"));
        let result = run(&[
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--batch-bytes",
            "1",
            "--block-size",
            "2",
            "--merge-fan-in",
            fan_in,
            "--merge-threads",
            "2",
            "--sort",
            "cpu",
            "--keep-intermediate",
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            String::from_utf8_lossy(&result.stdout)
                .contains("Final merge + compression: 17 postings, 11 terms")
        );
        let work = output.join("_work");
        let directories = fs::read_dir(&work)
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().path().is_dir())
            .count();
        // 5 inputs fit fan-in 128. For fan-in 2, preparation stops at 2 inputs.
        assert_eq!(directories, if fan_in == "128" { 0 } else { 2 });
        assert!(!work.join("merged.bin").exists());
        outputs.push(output);
    }
    for file in [
        "postings.bin",
        "lexicon.bin",
        "page_table.bin",
        "blocks.bin",
    ] {
        assert_eq!(
            fs::read(outputs[0].join(file)).unwrap(),
            fs::read(outputs[1].join(file)).unwrap()
        );
    }
}

#[cfg(not(feature = "gpu-sort"))]
#[test]
fn gpu_sort_requires_feature_and_invalid_sort_options_do_not_create_outputs() {
    let root = TestDir::new("sort-options");
    let input = root.0.join("input.tsv");
    fs::write(&input, "7\trust search\n").unwrap();
    for mode in ["gpu", "invalid"] {
        let output = root.0.join(mode);
        let result = run(&[
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--sort",
            mode,
        ]);
        assert!(!result.status.success());
        assert!(!output.exists());
        if mode == "gpu" {
            assert!(String::from_utf8_lossy(&result.stderr).contains("--features gpu-sort"));
        }
    }
}

#[cfg(feature = "gpu-sort")]
#[test]
#[ignore = "Requires a hardware Metal GPU; run with --features gpu-sort -- --ignored"]
fn gpu_sorted_runs_produce_identical_final_index_with_multiple_workers() {
    let root = TestDir::new("gpu-equivalence");
    let input = root.0.join("input.tsv");
    let contents: String = (0..640)
        .map(|i| {
            let text = if i % 31 == 0 {
                String::new()
            } else {
                format!("Rust rust Café naïve 東京 term{}", i % 53)
            };
            format!("{}\t{text}\n", i * 11 + 7)
        })
        .collect();
    fs::write(&input, contents).unwrap();
    let mut outputs = Vec::new();
    for (mode, threads) in [("cpu", "1"), ("gpu", "1"), ("gpu", "4")] {
        let output = root.0.join(format!("{mode}-{threads}"));
        let result = run(&[
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--sort",
            mode,
            "--threads",
            threads,
            "--batch-bytes",
            "4096",
            "--merge-fan-in",
            "3",
            "--block-size",
            "2",
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if mode == "gpu" {
            assert!(String::from_utf8_lossy(&result.stdout).contains("via Metal, radix sort"));
            assert!(String::from_utf8_lossy(&result.stdout).contains("Sort GPU:"));
            assert!(
                String::from_utf8_lossy(&result.stdout)
                    .contains("writer follows sorted indices (no posting reorder)")
            );
        }
        assert!(!output.join("_work").exists());
        outputs.push(output);
    }
    for name in [
        "postings.bin",
        "lexicon.bin",
        "page_table.bin",
        "blocks.bin",
    ] {
        let expected = fs::read(outputs[0].join(name)).unwrap();
        for output in &outputs[1..] {
            assert_eq!(expected, fs::read(output.join(name)).unwrap(), "{name}");
        }
    }
}
