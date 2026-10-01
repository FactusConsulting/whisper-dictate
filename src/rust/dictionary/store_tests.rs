use super::{
    add_replacement, add_term, load_dictionary_document, terms_from_document, write_terms,
};

#[test]
fn atomic_dictionary_updates_keep_terms_replacements_and_unknown_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dictionary.json");
    std::fs::write(
        &path,
        r#"{"terms":["Rust"],"replacements":{"wd":"WD"},"metadata":{"keep":true}}"#,
    )
    .unwrap();
    add_term(&path, "Windows").unwrap();
    add_replacement(&path, "cli=CLI").unwrap();
    let document = load_dictionary_document(&path).unwrap();
    assert_eq!(document["metadata"]["keep"], true);
    assert_eq!(document["replacements"]["wd"], "WD");
    assert_eq!(document["replacements"]["cli"], "CLI");
    assert_eq!(terms_from_document(&document), ["Rust", "Windows"]);
    write_terms(&path, &["Whisper".to_owned()], Some(document)).unwrap();
    let updated = load_dictionary_document(&path).unwrap();
    assert_eq!(terms_from_document(&updated), ["Whisper"]);
    assert_eq!(updated["replacements"]["wd"], "WD");
    assert_eq!(updated["replacements"]["cli"], "CLI");
    assert_eq!(updated["metadata"]["keep"], true);
}

#[cfg(windows)]
#[test]
fn locked_dictionary_cannot_be_replaced_and_keeps_last_good_bytes() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dictionary.json");
    let previous = r#"{"terms":["Rust"],"replacements":{"wd":"WD"},"unknown":true}"#;
    std::fs::write(&path, previous).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(add_term(&path, "Windows").is_err());
    assert!(add_replacement(&path, "cli=CLI").is_err());
    drop(lock);
    assert_eq!(std::fs::read_to_string(path).unwrap(), previous);
}
