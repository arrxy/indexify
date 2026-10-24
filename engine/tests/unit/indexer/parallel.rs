use super::*;
use crate::tokenizer::SimpleTokenizer;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn worker_write_error_is_returned_without_blocking_the_producer() {
    let name = format!(
        "run-worker-error-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(name);
    fs::create_dir(&root).unwrap();
    let input = root.join("input.tsv");
    fs::write(
        &input,
        (0..1000)
            .map(|i| format!("{i}\trust search\n"))
            .collect::<String>(),
    )
    .unwrap();
    let output = root.join("runs");
    fs::create_dir(&output).unwrap();
    fs::create_dir(output.join("run_0001.bin")).unwrap();
    let result = build_runs_parallel(
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        1,
        &SimpleTokenizer,
        4,
        &super::super::sorting::CpuSorter,
    );
    assert!(result.is_err());
    fs::remove_dir_all(root).unwrap();
}
