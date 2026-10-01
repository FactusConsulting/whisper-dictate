//! panic diagnostic regressions.

use crate::diag::{format_panic_report, install_gui_diagnostic_log, install_gui_panic_hook};
use std::process::Command;

#[test]
fn panic_report_keeps_message_and_source_location() {
    let text = format_panic_report(&"whisper worker failed", Some(("runtime.rs", 42, 7)));
    assert_eq!(
        text,
        "[panic] Rust panic thread=diag_tests::panic_tests::panic_report_keeps_message_and_source_location at runtime.rs:42:7: whisper worker failed"
    );
}

#[test]
fn panic_report_handles_non_string_payload_without_losing_the_marker() {
    let text = format_panic_report(&42_u8, None);
    assert_eq!(
        text,
        "[panic] Rust panic thread=diag_tests::panic_tests::panic_report_handles_non_string_payload_without_losing_the_marker: non-string panic payload"
    );
}

#[test]
fn panic_report_escapes_line_breaks() {
    let text = format_panic_report(&"first\r\nsecond\nthird", None);
    assert!(
        text.ends_with("first\\r\\nsecond\\nthird"),
        "panic payload line breaks must stay visible without splitting the record: {text:?}"
    );
    assert!(
        !text.contains('\r') && !text.contains('\n'),
        "panic report must remain one physical diagnostic line: {text:?}"
    );
}

/// Run the real process-wide hook in a child test process: a panic hook cannot
/// be reset safely in this test binary, and this preserves the previous
/// default hook for the delegation assertion.
#[test]
fn installed_panic_hook_records_and_delegates_in_child_process() {
    if std::env::var_os("WD_PANIC_HOOK_CHILD_LOG").is_some() {
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("panic-hook.log");
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "diag_tests::panic_tests::panic_hook_child_records_and_delegates",
            "--nocapture",
        ])
        .env("WD_PANIC_HOOK_CHILD_LOG", &log_path)
        .output()
        .expect("run isolated panic-hook test");
    assert!(
        output.status.success(),
        "child panic-hook test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let contents = std::fs::read_to_string(&log_path).expect("read panic diagnostic log");
    assert!(
        contents.contains(
            "[panic] Rust panic thread=diag_tests::panic_tests::panic_hook_child_records_and_delegates"
        ),
        "installed hook must write a named panic record: {contents:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("panic-hook-child"),
        "previous panic hook must still receive the panic: {stderr}"
    );
}

#[test]
fn panic_hook_child_records_and_delegates() {
    let Some(log_path) = std::env::var_os("WD_PANIC_HOOK_CHILD_LOG") else {
        return;
    };
    let log_path = std::path::PathBuf::from(log_path);
    install_gui_diagnostic_log(&log_path).expect("install child diagnostic log");
    install_gui_panic_hook();
    let result = std::panic::catch_unwind(|| panic!("panic-hook-child"));
    assert!(result.is_err(), "the child must catch its deliberate panic");
    assert!(
        crate::diag::drain_panic_reports(std::time::Duration::from_millis(500)),
        "the teardown drain must acknowledge the queued panic record"
    );
}
