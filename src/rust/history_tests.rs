use super::*;

#[test]
fn missing_and_empty_history_have_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    for exists in [false, true] {
        if exists {
            fs::write(&path, b"").unwrap();
        }
        assert!(last_row(&path).unwrap().is_none());
        assert!(last_n(&path, 0).unwrap().is_empty());
        assert!(search_rows(&path, "text", 0).unwrap().is_empty());
    }
}

#[test]
fn unicode_and_final_partial_rows_are_handled_independently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::write(&path, "{\"text\":\"hej æøå 日本語\"}\n{\"text\":").unwrap();
    assert_eq!(last_row(&path).unwrap().unwrap()["text"], "hej æøå 日本語");
    assert_eq!(search_rows(&path, "日本", 10).unwrap().len(), 1);
}

#[test]
fn last_and_search_are_bounded_even_when_the_requested_limit_is_huge() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let mut file = fs::File::create(&path).unwrap();
    for index in 0..1100 {
        writeln!(
            file,
            "{}",
            serde_json::json!({"text": format!("entry {index}")})
        )
        .unwrap();
    }
    drop(file);
    let last = last_n(&path, usize::MAX).unwrap();
    let search = search_rows(&path, "ENTRY", usize::MAX).unwrap();
    assert_eq!(last.len(), 1000);
    assert_eq!(search.len(), 1000);
    assert_eq!(last[0]["text"], "entry 1099");
    assert_eq!(last.last().unwrap()["text"], "entry 100");
    assert_eq!(last, search);
}

#[test]
fn corrupt_utf8_does_not_prevent_recovering_the_last_valid_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    fs::write(&path, b"\xff\n{\"text\":\"recoverable\"}\n").unwrap();
    assert_eq!(last_row(&path).unwrap().unwrap()["text"], "recoverable");
    assert_eq!(search_rows(&path, "recoverable", 10).unwrap().len(), 1);
}
