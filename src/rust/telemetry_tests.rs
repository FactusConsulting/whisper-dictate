use super::*;
use std::{fs, io::Write};

#[test]
fn preview_counts_all_valid_rows_but_only_keeps_a_bounded_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(b"\xff\ninvalid\n").unwrap();
    for index in 0..1100 {
        writeln!(
            file,
            "{}",
            serde_json::json!({"text": format!("entry {index}")})
        )
        .unwrap();
    }
    drop(file);
    let preview = preview_jsonl(&path, usize::MAX).unwrap();
    assert_eq!(preview.total_rows, 1100);
    assert_eq!(preview.shown_rows, 1000);
    assert!(!preview.text.lines().any(|line| line.ends_with("entry 99")));
    assert!(preview.text.contains("entry 1099"));
}
