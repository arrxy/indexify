use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn sink_errors_and_truncated_input_propagate_without_deleting_sources() {
    let root = std::env::temp_dir().join(format!(
        "merge-sink-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let source = root.join("run.bin");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&4_u32.to_le_bytes());
    bytes.extend_from_slice(b"rust");
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    std::fs::write(&source, &bytes).unwrap();
    let mut calls = 0;
    let result = merge_into(&[source.clone()], |_, _, _| {
        calls += 1;
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sink failed",
        ))
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(calls, 1);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    std::fs::write(&source, &bytes[..bytes.len() - 1]).unwrap();
    let result = merge_into(&[source], |_, _, _| {
        panic!("Truncated posting must not be emitted")
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        merge_into(&[], |_, _, _| panic!("Empty inputs must emit nothing")).unwrap(),
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}
