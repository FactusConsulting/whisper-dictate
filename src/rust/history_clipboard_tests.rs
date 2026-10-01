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
