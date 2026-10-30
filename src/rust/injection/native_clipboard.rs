//! Guarded system clipboard shared by native dictation and paste-last.
//!
//! Paste injection saves the clipboard, writes the transcript, sends the
//! paste chord, and later restores the saved value. The string-only restore
//! path can only put text back, so [`GuardedClipboard`] refuses to overwrite
//! a selection it could not back up: a rich-format selection (image, HTML)
//! reads as `None`, and `write` is only honored after a successful `read`.
//! An empty clipboard is restorable as an empty string where the platform
//! can tell it apart from a non-text selection (Windows). macOS has no such
//! probe, so an empty clipboard there reads as `None` and paste mode falls
//! back to typing (or errors when paste was forced) until text is on it.

use super::paste::Clipboard;

/// Raw text clipboard surface the guard wraps. Kept narrow so tests can drive
/// the guard with a fake instead of the process-global clipboard. Linux
/// production routes through `system_clipboard` instead, so the guard is
/// test-only there.
#[cfg(any(not(target_os = "linux"), test))]
pub(crate) trait TextClipboardAccess {
    /// Current plain-text selection, or `None` when none is available.
    fn get_text(&mut self) -> Option<String>;
    /// Replace the clipboard with `value`; `true` on success.
    fn set_text(&mut self, value: &str) -> bool;
    /// `Some(true)` when every present format is plain text the string-only
    /// restore can retain, `Some(false)` when a rich format is present, `None`
    /// when the clipboard could not be inspected.
    fn has_only_text_formats(&mut self) -> Option<bool>;
    /// `Some(true)` when the clipboard holds no formats at all, `None` when
    /// that could not be determined.
    fn is_empty(&mut self) -> Option<bool>;
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
impl<C: TextClipboardAccess> Clipboard for GuardedClipboard<C> {
    fn read(&mut self) -> Option<String> {
        self.readable = false;
        match self.inner.get_text() {
            Some(value) => {
                if self.inner.has_only_text_formats() != Some(true) {
                    // Replacing a rich selection with plain text would lose
                    // formats that the string-only restore path cannot retain.
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

    fn write(&mut self, value: &str) -> bool {
        self.readable && self.inner.set_text(value)
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
impl TextClipboardAccess for ArboardClipboard {
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
    // CF_TEXT, CF_OEMTEXT, CF_UNICODETEXT, and CF_LOCALE are restorable by the
    // string-only clipboard adapter; rich and application-defined formats are not.
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
