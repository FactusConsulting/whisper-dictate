//! Guarded system clipboard shared by native dictation and paste-last.
//!
//! Paste injection saves the clipboard, writes the transcript, sends the
//! paste chord, and later restores the saved value. [`GuardedClipboard`]
//! refuses to overwrite a selection it could not back up: `write` is only
//! honored after a successful `read` or `backup`. A plain-text selection is
//! saved as text; a rich selection (HTML, RTF, images) is saved format by
//! format on Windows and restored in full. A selection with formats that
//! cannot be saved (private or GDI-object formats, very large data) is left
//! untouched. An empty clipboard is restorable as an empty string where the
//! platform can tell it apart from a non-text selection (Windows). macOS has
//! no such probe and no format snapshot, so an empty or rich clipboard there
//! cannot be backed up and paste mode falls back to typing (or errors when
//! paste was forced) until text is on it.

use super::paste::Clipboard;
#[cfg(any(not(target_os = "linux"), test))]
use super::paste::{ClipboardBackup, SavedFormat};

/// Raw clipboard surface the guard wraps. Kept narrow so tests can drive the
/// guard with a fake instead of the process-global clipboard. Linux
/// production routes through `system_clipboard` instead, so the guard is
/// test-only there.
#[cfg(any(not(target_os = "linux"), test))]
pub(crate) trait RawClipboardAccess {
    /// Current plain-text selection, or `None` when none is available.
    fn get_text(&mut self) -> Option<String>;
    /// Replace the clipboard with `value`; `true` on success.
    fn set_text(&mut self, value: &str) -> bool;
    /// `Some(true)` when every present format is plain text a text backup
    /// can retain, `Some(false)` when a rich format is present, `None` when
    /// the clipboard could not be inspected.
    fn has_only_text_formats(&mut self) -> Option<bool>;
    /// `Some(true)` when the clipboard holds no formats at all, `None` when
    /// that could not be determined.
    fn is_empty(&mut self) -> Option<bool>;
    /// Every format of the current selection, or `None` when any of them
    /// cannot be saved.
    fn snapshot_formats(&mut self) -> Option<Vec<SavedFormat>>;
    /// Replace the clipboard with `formats`; `true` when all were set.
    fn restore_formats(&mut self, formats: &[SavedFormat]) -> bool;
}

/// [`Clipboard`] adapter that only writes over selections it can restore.
#[cfg(any(not(target_os = "linux"), test))]
pub(crate) struct GuardedClipboard<C> {
    inner: C,
    readable: bool,
}

#[cfg(any(not(target_os = "linux"), test))]
impl<C> GuardedClipboard<C> {
    pub(crate) fn new(inner: C) -> Self {
        Self {
            inner,
            readable: false,
        }
    }
}

#[cfg(any(not(target_os = "linux"), test))]
impl<C: RawClipboardAccess> GuardedClipboard<C> {
    /// Accept `text` as a restorable plain-text selection, or `None` when the
    /// clipboard holds more than plain text or could not be read.
    fn accept_plain_text(&mut self, text: Option<String>) -> Option<String> {
        self.readable = false;
        match text {
            Some(value) => {
                if self.inner.has_only_text_formats() != Some(true) {
                    // Replacing a rich selection with plain text would lose
                    // formats a text restore cannot put back.
                    return None;
                }
                self.readable = true;
                Some(value)
            }
            None if self.inner.is_empty() == Some(true) => {
                // An empty clipboard has nothing to restore, but it is safe
                // to replace and later restore as an empty string.
                self.readable = true;
                Some(String::new())
            }
            None => None,
        }
    }
}

#[cfg(any(not(target_os = "linux"), test))]
impl<C: RawClipboardAccess> Clipboard for GuardedClipboard<C> {
    fn read(&mut self) -> Option<String> {
        let text = self.inner.get_text();
        self.accept_plain_text(text)
    }

    fn write(&mut self, value: &str) -> bool {
        self.readable && self.inner.set_text(value)
    }

    fn backup(&mut self) -> Option<ClipboardBackup> {
        let text = self.inner.get_text();
        if let Some(plain) = self.accept_plain_text(text.clone()) {
            return Some(ClipboardBackup::Text(plain));
        }
        let formats = self.inner.snapshot_formats()?;
        self.readable = true;
        Some(ClipboardBackup::Formats { text, formats })
    }

    fn restore(&mut self, backup: &ClipboardBackup) -> bool {
        match backup {
            ClipboardBackup::Text(text) => self.write(text),
            ClipboardBackup::Formats { formats, .. } => self.inner.restore_formats(formats),
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct ArboardClipboard {
    inner: arboard::Clipboard,
}

#[cfg(not(target_os = "linux"))]
impl ArboardClipboard {
    fn new() -> Result<Self, String> {
        arboard::Clipboard::new()
            .map(|inner| Self { inner })
            .map_err(|error| format!("system clipboard initialization failed: {error}"))
    }
}

#[cfg(not(target_os = "linux"))]
impl RawClipboardAccess for ArboardClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.inner.get_text().ok()
    }

    fn set_text(&mut self, value: &str) -> bool {
        self.inner.set_text(value.to_owned()).is_ok()
    }

    fn has_only_text_formats(&mut self) -> Option<bool> {
        #[cfg(target_os = "windows")]
        {
            clipboard_has_only_text_formats()
        }
        #[cfg(not(target_os = "windows"))]
        {
            // Format inspection is Windows-only; elsewhere a successful text
            // read is the only signal available and is accepted as-is.
            Some(true)
        }
    }

    fn is_empty(&mut self) -> Option<bool> {
        #[cfg(target_os = "windows")]
        {
            clipboard_is_empty()
        }
        #[cfg(not(target_os = "windows"))]
        {
            // arboard reports "empty" and "non-text selection" with the same
            // error, so emptiness is unknown and the guard refuses the write.
            None
        }
    }

    fn snapshot_formats(&mut self) -> Option<Vec<SavedFormat>> {
        #[cfg(target_os = "windows")]
        {
            super::clipboard_formats::snapshot()
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }

    fn restore_formats(&mut self, formats: &[SavedFormat]) -> bool {
        #[cfg(target_os = "windows")]
        {
            super::clipboard_formats::restore(formats)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = formats;
            false
        }
    }
}

#[cfg(target_os = "windows")]
fn clipboard_is_empty() -> Option<bool> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, CountClipboardFormats, OpenClipboard,
    };

    // The text read has already released arboard's clipboard handle. Open it
    // briefly to distinguish an empty clipboard from a non-text selection.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let empty = CountClipboardFormats() == 0;
        CloseClipboard();
        Some(empty)
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn clipboard_has_only_text_formats() -> Option<bool> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EnumClipboardFormats, OpenClipboard,
    };

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let mut format = 0;
        let mut only_text = true;
        while {
            format = EnumClipboardFormats(format);
            format != 0
        } {
            if !is_text_clipboard_format(format) {
                only_text = false;
                break;
            }
        }
        CloseClipboard();
        Some(only_text)
    }
}

#[cfg(any(target_os = "windows", test))]
fn is_text_clipboard_format(format: u32) -> bool {
    // CF_TEXT, CF_OEMTEXT, CF_UNICODETEXT, and CF_LOCALE are retained by a text
    // backup; rich and application-defined formats need a format snapshot.
    matches!(format, 1 | 7 | 13 | 16)
}

/// System clipboard for paste injection on the current platform.
pub(crate) fn platform_clipboard() -> Result<Box<dyn Clipboard + Send>, String> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(super::system_clipboard::SystemClipboard::default()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        ArboardClipboard::new().map(|clipboard| Box::new(GuardedClipboard::new(clipboard)) as _)
    }
}

#[cfg(test)]
#[path = "native_clipboard_tests.rs"]
mod tests;
