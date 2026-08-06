//! Native reinjection used by the floating UI's transcript actions.

use anyhow::{anyhow, Result};
use std::sync::{Arc, OnceLock};

use crate::dictate::backends::EnigoInjectBackend;
use crate::injection::{InjectMethod, Injector, LinuxSession};

#[cfg(not(target_os = "linux"))]
struct ArboardClipboard {
    inner: arboard::Clipboard,
    readable: bool,
}

#[cfg(not(target_os = "linux"))]
impl ArboardClipboard {
    fn new() -> Result<Self, String> {
        arboard::Clipboard::new()
            .map(|inner| Self {
                inner,
                readable: false,
            })
            .map_err(|error| format!("system clipboard initialization failed: {error}"))
    }
}

#[cfg(not(target_os = "linux"))]
impl crate::injection::Clipboard for ArboardClipboard {
    fn read(&mut self) -> Option<String> {
        self.readable = false;
        match self.inner.get_text() {
            Ok(value) => {
                #[cfg(target_os = "windows")]
                if clipboard_has_only_text_formats() != Some(true) {
                    // Replacing a rich selection with plain text would lose
                    // formats that the string-only restore path cannot retain.
                    return None;
                }
                self.readable = true;
                Some(value)
            }
            Err(_) if clipboard_is_empty() == Some(true) => {
                // An empty clipboard has nothing to restore, but it is safe
                // to replace and later restore as an empty string.
                self.readable = true;
                Some(String::new())
            }
            Err(_) => None,
        }
    }

    fn write(&mut self, value: &str) -> bool {
        self.readable && self.inner.set_text(value.to_owned()).is_ok()
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
fn clipboard_has_only_text_formats() -> Option<bool> {
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

#[cfg(not(target_os = "windows"))]
fn clipboard_is_empty() -> Option<bool> {
    None
}

fn platform_clipboard() -> Result<Box<dyn crate::injection::Clipboard + Send>, String> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(
            crate::injection::system_clipboard::SystemClipboard::default(),
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        ArboardClipboard::new().map(|clipboard| Box::new(clipboard) as _)
    }
}

fn auto_method(text: &str) -> InjectMethod {
    #[cfg(target_os = "linux")]
    let session = LinuxSession::detect();
    #[cfg(not(target_os = "linux"))]
    let session = LinuxSession::Unknown;
    auto_method_for(text, std::env::consts::OS, session)
}

fn auto_method_for(text: &str, os: &str, session: LinuxSession) -> InjectMethod {
    if os == "windows"
        || (os == "linux"
            && !text.is_ascii()
            && matches!(
                session,
                LinuxSession::OtherWayland | LinuxSession::KdeWayland
            ))
    {
        InjectMethod::Paste(None)
    } else {
        InjectMethod::Typing
    }
}

fn resolve_method(mode: &str, text: &str) -> Result<InjectMethod> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "paste" => Ok(InjectMethod::Paste(None)),
        "type" => Ok(InjectMethod::Typing),
        "print" => Err(anyhow!(
            "the last transcript used print mode and was not injected"
        )),
        "" | "auto" => Ok(auto_method(text)),
        _ => Ok(auto_method(text)),
    }
}

static UI_BACKEND: OnceLock<Result<Arc<EnigoInjectBackend>, String>> = OnceLock::new();

fn shared_backend() -> Result<Arc<EnigoInjectBackend>> {
    UI_BACKEND
        .get_or_init(|| {
            let clipboard = platform_clipboard()?;
            Ok(Arc::new(
                EnigoInjectBackend::new(Injector::new(), InjectMethod::Typing)
                    .with_clipboard(clipboard),
            ))
        })
        .as_ref()
        .map(Arc::clone)
        .map_err(|error| anyhow!(error.clone()))
}

/// Activate the captured target, then inject without writing a plan or
/// transcript to stdout. `EnigoInjectBackend` supplies the runtime's global
/// self-injection guard and stale-modifier cleanup.
pub(crate) fn reinject_text(
    text: &str,
    mode: &str,
    target_title: &str,
    target_process: &str,
    target_id: &str,
) -> Result<()> {
    let method = resolve_method(mode, text)?;
    let xkb_layout = std::env::var("VOICEPI_XKB_LAYOUT")
        .ok()
        .filter(|layout| !layout.trim().is_empty());
    let backend = shared_backend()?;
    backend.set_target(target_title, target_process);
    backend.set_xkb_layout(xkb_layout.as_deref().unwrap_or_default());

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        crate::platform::window_enumeration::activate_window_with_id(
            target_id,
            target_title,
            target_process,
        )
        .map_err(|error| anyhow!(error))?;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    backend
        .inject_using(text, method)
        .map_err(|error| anyhow!(error.to_string()))
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
