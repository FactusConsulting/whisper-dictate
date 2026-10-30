use super::{is_text_clipboard_format, GuardedClipboard, TextClipboardAccess};
use crate::injection::Clipboard;

/// Fake raw clipboard: `text` is what `get_text` returns, `only_text` and
/// `empty` are the format probes, `written` records accepted writes.
struct FakeAccess {
    text: Option<String>,
    only_text: Option<bool>,
    empty: Option<bool>,
    written: Vec<String>,
}

impl FakeAccess {
    fn plain(text: &str) -> Self {
        Self {
            text: Some(text.to_owned()),
            only_text: Some(true),
            empty: Some(false),
            written: Vec::new(),
        }
    }
}

impl TextClipboardAccess for FakeAccess {
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
    let access = FakeAccess {
        text: None,
        only_text: None,
        empty: Some(true),
        written: Vec::new(),
    };
    let mut clipboard = GuardedClipboard::new(access);
    assert_eq!(clipboard.read().as_deref(), Some(""));
    assert!(clipboard.write("transcript"));
    assert_eq!(clipboard.inner.written, vec!["transcript".to_owned()]);
}

#[test]
fn unreadable_non_empty_clipboard_reads_as_none() {
    let access = FakeAccess {
        text: None,
        only_text: None,
        empty: Some(false),
        written: Vec::new(),
    };
    let mut clipboard = GuardedClipboard::new(access);
    assert_eq!(clipboard.read(), None);
    assert!(!clipboard.write("transcript"));
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
