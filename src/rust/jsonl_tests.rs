use super::*;
use std::io::Write;

#[test]
fn oversized_and_invalid_rows_do_not_hide_later_valid_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let mut file = File::create(&path).unwrap();
    file.write_all(b"{\"text\":\"first\"}\r\n").unwrap();
    file.write_all(&vec![b'x'; MAX_ROW_BYTES + 100]).unwrap();
    file.write_all(b"\n\xff\n{broken}\n{\"text\":\"last\"}")
        .unwrap();
    drop(file);
    let mut rows = Vec::new();
    scan(&path, |value, _| rows.push(value)).unwrap();
    assert_eq!(
        rows.iter()
            .map(|value| value["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "last"]
    );
}

#[test]
fn requested_tail_size_and_aggregate_bytes_are_bounded() {
    let mut tail = Tail::new(usize::MAX);
    for index in 0..MAX_TAIL_ROWS + 10 {
        tail.push(serde_json::json!({"id": index}), 10);
    }
    let rows = tail.into_rows();
    assert_eq!(rows.len(), MAX_TAIL_ROWS);
    assert_eq!(rows[0]["id"], 10);
    let mut tail = Tail::new(1000);
    for index in 0..20 {
        tail.push(serde_json::json!({"id": index}), MAX_ROW_BYTES);
    }
    let rows = tail.into_rows();
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[0]["id"], 12);
}
