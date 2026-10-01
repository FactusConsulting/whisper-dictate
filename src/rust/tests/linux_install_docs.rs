mod common;

#[test]
fn linux_source_install_documentation_matches_wrapper_and_payload_paths() {
    let root = common::repo_root();
    let script = std::fs::read_to_string(root.join("scripts/linux/install-rust-ui.sh")).unwrap();
    let docs = std::fs::read_to_string(root.join("docs/INSTALLATION.md")).unwrap();
    assert!(script.contains("BIN_DIR=\"${HOME}/.local/bin\""));
    assert!(script.contains("LIB_DIR=\"${HOME}/.local/lib/whisper-dictate\""));
    assert!(script.contains("BIN=\"${BIN_DIR}/wd\""));
    assert!(script.contains("REAL_BIN=\"${LIB_DIR}/wd-app\""));
    assert!(docs.contains("`~/.local/bin/wd`"));
    assert!(docs.contains("`~/.local/lib/whisper-dictate/wd-app`"));
    assert!(!docs.contains("`~/.local/bin/whisper-dictate`"));
}
