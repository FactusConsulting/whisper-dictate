use super::{is_text_clipboard_format, GuardedClipboard, RawClipboardAccess};
use crate::injection::{Clipboard, ClipboardBackup, SavedFormat, SavedFormatKind};

/// Fake raw clipboard: `text` is what `get_text` returns, `only_text` and
/// `empty` are the format probes, `snapshot` is what a format snapshot
/// yields, `written` and `restored` record accepted writes and restores.
struct FakeAccess {
    text: Option<String>,
    only_text: Option<bool>,
    empty: Option<bool>,
    snapshot: Option<Vec<SavedFormat>>,
    written: Vec<String>,
    restored: Vec<Vec<SavedFormat>>,
}

impl FakeAccess {
    fn plain(text: &str) -> Self {
        Self {
            text: Some(text.to_owned()),
            only_text: Some(true),
            empty: Some(false),
            snapshot: None,
            written: Vec::new(),
            restored: Vec::new(),
        }
    }

    fn unreadable(empty: Option<bool>) -> Self {
        Self {
            text: None,
            only_text: None,
            empty,
            ..Self::plain("")
        }
    }
}

fn html_formats() -> Vec<SavedFormat> {
    vec![
        SavedFormat {
            format: 0xC0F1,
            kind: SavedFormatKind::Memory,
            bytes: b"<b>caption</b>\0".to_vec(),
        },
        SavedFormat {
            format: 13,
            kind: SavedFormatKind::Memory,
            bytes: vec![b'c', 0, 0, 0],
        },
    ]
}

impl RawClipboardAccess for FakeAccess {
    fn get_text(&mut self) -> Option<String> {
        self.text.clone()
    }

    fn set_text(&mut self, value: &str) -> bool {
        self.written.push(value.to_owned());
        true
    }

    fn has_only_text_formats(&mut self) -> Option<bool> {
        self.only_text
    }

    fn is_empty(&mut self) -> Option<bool> {
        self.empty
    }

    fn snapshot_formats(&mut self) -> Option<Vec<SavedFormat>> {
        self.snapshot.clone()
    }

    fn restore_formats(&mut self, formats: &[SavedFormat]) -> bool {
        self.restored.push(formats.to_vec());
        true
    }
}

#[test]
fn write_is_refused_before_a_successful_read() {
    let mut clipboard = GuardedClipboard::new(FakeAccess::plain("backup"));
    assert!(!clipboard.write("transcript"));
    assert!(clipboard.inner.written.is_empty());

    assert_eq!(clipboard.read().as_deref(), Some("backup"));
    assert!(clipboard.write("transcript"));
    assert_eq!(clipboard.inner.written, vec!["transcript".to_owned()]);
}

#[test]
fn rich_format_selection_reads_as_none_and_blocks_write() {
    let mut access = FakeAccess::plain("caption of an image");
    access.only_text = Some(false);
    let mut clipboard = GuardedClipboard::new(access);
    assert_eq!(clipboard.read(), None);
    assert!(!clipboard.write("transcript"));
    assert!(clipboard.inner.written.is_empty());
}

#[test]
fn uninspectable_formats_block_write() {
    let mut access = FakeAccess::plain("text");
    access.only_text = None;
    let mut clipboard = GuardedClipboard::new(access);
    assert_eq!(clipboard.read(), None);
    assert!(!clipboard.write("transcript"));
}

#[test]
fn empty_clipboard_reads_as_empty_string_and_allows_write() {
    let access = FakeAccess::unreadable(Some(true));
    let mut clipboard = GuardedClipboard::new(access);
    assert_eq!(clipboard.read().as_deref(), Some(""));
    assert!(clipboard.write("transcript"));
    assert_eq!(clipboard.inner.written, vec!["transcript".to_owned()]);
}

#[test]
fn unreadable_non_empty_clipboard_reads_as_none() {
    let access = FakeAccess::unreadable(Some(false));
    let mut clipboard = GuardedClipboard::new(access);
    assert_eq!(clipboard.read(), None);
    assert!(!clipboard.write("transcript"));
}

/// Without an emptiness probe (macOS: arboard reports an empty clipboard
/// and a non-text selection with the same error) a failed read cannot be
/// told apart from a rich selection, so the write is refused and paste
/// falls back to typing rather than risk clobbering an unrestorable
/// selection.
#[test]
fn failed_read_with_unknown_emptiness_refuses_write() {
    let access = FakeAccess::unreadable(None);
    let mut clipboard = GuardedClipboard::new(access);
    assert_eq!(clipboard.read(), None);
    assert!(!clipboard.write("transcript"));
    assert!(clipboard.inner.written.is_empty());
}

#[test]
fn failed_read_revokes_earlier_write_permission() {
    let mut clipboard = GuardedClipboard::new(FakeAccess::plain("backup"));
    assert!(clipboard.read().is_some());
    clipboard.inner.only_text = Some(false);
    assert_eq!(clipboard.read(), None);
    assert!(!clipboard.write("transcript"));
}

#[test]
fn clipboard_backup_accepts_only_plain_text_formats() {
    assert!(is_text_clipboard_format(1));
    assert!(is_text_clipboard_format(7));
    assert!(is_text_clipboard_format(13));
    assert!(is_text_clipboard_format(16));
    assert!(!is_text_clipboard_format(49324));
}

#[test]
fn rich_selection_backup_saves_every_format_and_allows_write() {
    let mut access = FakeAccess::plain("caption");
    access.only_text = Some(false);
    access.snapshot = Some(html_formats());
    let mut clipboard = GuardedClipboard::new(access);

    assert_eq!(
        clipboard.backup(),
        Some(ClipboardBackup::Formats {
            text: Some("caption".to_owned()),
            formats: html_formats(),
        })
    );
    assert!(clipboard.write("transcript"));
    assert_eq!(clipboard.inner.written, vec!["transcript".to_owned()]);
}

#[test]
fn non_text_selection_backup_carries_no_text() {
    let mut access = FakeAccess::unreadable(Some(false));
    access.snapshot = Some(html_formats());
    let mut clipboard = GuardedClipboard::new(access);

    let backup = clipboard.backup().expect("format snapshot");
    assert_eq!(backup.text(), None);
    assert!(clipboard.write("transcript"));
}

#[test]
fn unsavable_rich_selection_refuses_backup_and_write() {
    let mut access = FakeAccess::plain("caption");
    access.only_text = Some(false);
    let mut clipboard = GuardedClipboard::new(access);

    assert_eq!(clipboard.backup(), None);
    assert!(!clipboard.write("transcript"));
    assert!(clipboard.inner.written.is_empty());
}

#[test]
fn plain_text_backup_does_not_snapshot_formats() {
    let mut access = FakeAccess::plain("plain");
    access.snapshot = Some(html_formats());
    let mut clipboard = GuardedClipboard::new(access);

    assert_eq!(
        clipboard.backup(),
        Some(ClipboardBackup::Text("plain".to_owned()))
    );
}

#[test]
fn restore_replays_saved_formats_and_writes_saved_text() {
    let mut clipboard = GuardedClipboard::new(FakeAccess::plain("transcript"));
    let rich = ClipboardBackup::Formats {
        text: Some("caption".to_owned()),
        formats: html_formats(),
    };
    assert!(clipboard.restore(&rich));
    assert_eq!(clipboard.inner.restored, vec![html_formats()]);

    assert!(clipboard.read().is_some());
    assert!(clipboard.restore(&ClipboardBackup::Text("prior".to_owned())));
    assert_eq!(clipboard.inner.written, vec!["prior".to_owned()]);
}
