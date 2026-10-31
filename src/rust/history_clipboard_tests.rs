#[test]
fn clipboard_writer_delivers_eof_before_waiting_for_helper_exit() {
    super::run_command(
        crate::bounded_process::fixtures::command("echo"),
        "selection æøå",
    )
    .unwrap();
}

#[test]
fn unsuccessful_clipboard_helper_is_reported_without_publishing_success() {
    let error = super::run_command(
        crate::bounded_process::fixtures::command("fail"),
        "selection",
    )
    .unwrap_err();
    assert!(error.to_string().contains("exit status") || error.to_string().contains("pipe"));
}

#[test]
fn unicode_clipboard_payload_is_bomless_nul_terminated_utf16() {
    assert_eq!(
        super::unicode_text_units("æøå 😀"),
        [
            0x00E6, // æ
            0x00F8, // ø
            0x00E5, // å
            0x0020, // space
            0xD83D, 0xDE00, // 😀 as a surrogate pair
            0x0000, // CF_UNICODETEXT terminator
        ]
    );
}
