//! Paste cycles over a rich (multi-format) clipboard selection: the full
//! selection must come back after the transcript is pasted.

use std::time::{Duration, Instant};

use super::inject_test_support::{
    backend_with_clipboard, backend_with_clipboard_and_delay, RecordingBackend, RecordingClipboard,
};
use crate::dictate::session::types::InjectBackend;
use crate::injection::{InjectMethod, PasteShortcut, SavedFormat, SavedFormatKind};

fn formats(tag: &[u8]) -> Vec<SavedFormat> {
    vec![
        SavedFormat {
            format: 0xC0F1,
            kind: SavedFormatKind::Memory,
            bytes: tag.to_vec(),
        },
        SavedFormat {
            format: 14,
            kind: SavedFormatKind::EnhancedMetafile,
            bytes: vec![1, 0, 0, 0],
        },
    ]
}

fn wait_for_format_restore(clipboard: &RecordingClipboard) -> Vec<Vec<SavedFormat>> {
    let deadline = Instant::now() + Duration::from_secs(1);
    while clipboard.restored_formats().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    clipboard.restored_formats()
}

#[test]
fn rich_selection_is_pasted_over_and_restored_in_full() {
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let clipboard = RecordingClipboard::with_rich_selection(Some("caption"), formats(b"html"));
    let handle = clipboard.clone();
    let backend = backend_with_clipboard(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
    );

    backend
        .inject("transcript")
        .expect("paste over a rich selection");

    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.starts_with("chord:")),
        "the transcript must be pasted, not typed"
    );
    assert_eq!(wait_for_format_restore(&handle), vec![formats(b"html")]);
    assert_eq!(handle.snapshot_writes(), ["transcript"]);
    assert_eq!(handle.read_contents().as_deref(), Some("caption"));
    assert_eq!(handle.rich_formats(), Some(formats(b"html")));
}

#[test]
fn rich_user_copy_between_overlapping_pastes_becomes_the_restore_target() {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("original"));
    let handle = clipboard.clone();
    let backend = backend_with_clipboard_and_delay(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
        Duration::from_millis(100),
    );

    backend.inject("first").expect("first paste ok");
    handle.simulate_rich_copy(None, formats(b"image"));
    backend.inject("second").expect("second paste ok");

    assert_eq!(wait_for_format_restore(&handle), vec![formats(b"image")]);
    assert_eq!(
        handle.snapshot_writes(),
        ["first", "second"],
        "the pre-burst text must not be restored over the newer rich copy"
    );
}

/// Transcript writes are plain text, so a rich selection is always the
/// user's newer copy, even when its plain-text form equals the transcript.
#[test]
fn rich_user_copy_matching_the_transcript_text_becomes_the_restore_target() {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("original"));
    let handle = clipboard.clone();
    let backend = backend_with_clipboard_and_delay(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
        Duration::from_millis(100),
    );

    backend.inject("first").expect("first paste ok");
    handle.simulate_rich_copy(Some("first"), formats(b"<b>first</b>"));
    backend.inject("second").expect("second paste ok");

    assert_eq!(
        wait_for_format_restore(&handle),
        vec![formats(b"<b>first</b>")]
    );
    assert_eq!(handle.snapshot_writes(), ["first", "second"]);
}
