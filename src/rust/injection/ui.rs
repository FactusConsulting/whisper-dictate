//! Native reinjection used by the floating UI's transcript actions.

use anyhow::{anyhow, Result};
#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};

use crate::dictate::backends::EnigoInjectBackend;
use crate::dictate::session::types::InjectError;
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

#[cfg(all(not(target_os = "windows"), not(target_os = "linux")))]
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

fn auto_mode_requested(mode: &str) -> bool {
    !matches!(
        mode.trim().to_ascii_lowercase().as_str(),
        "type" | "paste" | "print"
    )
}

fn should_fallback_auto_paste(
    mode: &str,
    method: InjectMethod,
    error: &InjectError,
    os: &str,
) -> bool {
    if !auto_mode_requested(mode) || !matches!(method, InjectMethod::Paste(_)) {
        return false;
    }
    // A refused clipboard write means the chord was never sent and nothing
    // was typed, so the typing fallback cannot double-inject. On Windows
    // this is the rich-selection case: the string-only clipboard adapter
    // refuses to replace a clipboard holding HTML/RTF with plain text, so
    // paste is impossible until the user copies text-only content — and
    // `auto` always picks paste on Windows, so the typing fallback is the
    // only safe outcome (Codex P2 injection/ui.rs:305).
    if os == "windows" {
        return matches!(error, InjectError::Backend(message)
            if message.starts_with("clipboard write failed"));
    }
    EnigoInjectBackend::is_safe_auto_fallback(error)
}

fn inject_using_resolved_method(
    backend: &EnigoInjectBackend,
    text: &str,
    mode: &str,
    method: InjectMethod,
) -> Result<(), InjectError> {
    match backend.inject_using(text, method) {
        Err(error) if should_fallback_auto_paste(mode, method, &error, std::env::consts::OS) => {
            backend.inject_using(text, InjectMethod::Typing)
        }
        result => result,
    }
}

static UI_BACKEND: OnceLock<Result<Arc<EnigoInjectBackend>, String>> = OnceLock::new();
static UI_TYPING_BACKEND: OnceLock<Arc<EnigoInjectBackend>> = OnceLock::new();
/// Backends whose clipboard-restore cycles the process-wide cancellation
/// and explicit-copy paths must be able to reach: the runtime session's
/// paste backend (`register_runtime_backend`) and the paste-last hotkey
/// worker's per-press backend (registered in `paste_last_into_focused_window`).
/// Entries without a pending restore are pruned by
/// [`register_runtime_backend`] and [`cancel_pending_clipboard_restore`]
/// so repeated hotkey presses do not accumulate.
static RUNTIME_BACKENDS: OnceLock<Mutex<Vec<Arc<EnigoInjectBackend>>>> = OnceLock::new();

pub(crate) fn register_runtime_backend(backend: &Arc<EnigoInjectBackend>) {
    let slot = RUNTIME_BACKENDS.get_or_init(|| Mutex::new(Vec::new()));
    let mut backends = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    backends.retain(|candidate| candidate.has_pending_restore());
    backends.push(Arc::clone(backend));
}

pub(crate) fn cancel_pending_clipboard_restore() {
    if let Some(Ok(backend)) = UI_BACKEND.get() {
        backend.cancel_pending_restore();
    }
    {
        if let Some(slot) = RUNTIME_BACKENDS.get() {
            let mut backends = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for backend in backends.iter() {
                backend.cancel_pending_restore();
            }
            // The most recently registered backend remains the live runtime
            // backend even when this copy found no pending restore. Keep it
            // registered so a later paste cycle can still be cancelled.
            let current = backends.last().cloned();
            backends.clear();
            if let Some(current) = current {
                backends.push(current);
            }
        }
    }
}

/// Claim the system clipboard only after a successful explicit write. Locks
/// every pending restore through that write, so a timer cannot replace the
/// new text in the gap between `set_text` and cancellation.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn copy_with_pending_restore_cancelled<T, E>(
    write: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let mut backends = Vec::new();
    if let Some(Ok(backend)) = UI_BACKEND.get() {
        backends.push(Arc::clone(backend));
    }
    if let Some(slot) = RUNTIME_BACKENDS.get() {
        let registered = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for backend in registered.iter() {
            if !backends.iter().any(|other| Arc::ptr_eq(other, backend)) {
                backends.push(Arc::clone(backend));
            }
        }
    }
    fn with_guards<T, E>(
        backends: &[Arc<EnigoInjectBackend>],
        write: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        if let Some((backend, remaining)) = backends.split_first() {
            backend.with_restore_guard(|| with_guards(remaining, write))
        } else {
            write()
        }
    }
    with_guards(&backends, write)
}

/// Inject `text` into whichever window currently has focus, without
/// touching any captured target. Used by the paste-last GUI hotkey: the
/// user presses the shortcut while the destination window already has
/// focus, so activating a stale captured target would be wrong.
///
/// Builds a fresh backend per call instead of reusing the shared UI
/// backend: the shared UI handles carry a captured reinject target and a
/// clipboard-restore cycle for the session flow, and a paste-last press
/// must neither read nor mutate that state. Construction is cheap
/// ([`Injector::new`] does no system calls; the enigo backend is built
/// lazily on first use), the keystroke burst is serialized against every
/// other injection path by the process-wide pipeline lock in
/// `EnigoInjectBackend::inject_using`, the guard brackets the burst
/// through the `inject_guard::global` slot that `install_hotkey`
/// populates, and `cancellation` (the runtime lifecycle flag) stops the
/// burst at the next character boundary once the user hits Stop.
///
/// The paste arm's backend is registered with
/// [`register_runtime_backend`] so `cancel_pending_clipboard_restore`
/// and `copy_with_pending_restore_cancelled` reach its restore cycle —
/// the same coordination the runtime session's paste backend gets.
///
/// The result carries the caller-facing error; the hotkey worker logs it
/// through `RuntimeEvent::Stderr` with ASCII escaping.
#[cfg(target_os = "windows")]
pub(crate) fn paste_last_into_focused_window(
    text: &str,
    mode: &str,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<()> {
    let method = resolve_method(mode, text)?;
    let backend = match method {
        InjectMethod::Typing => EnigoInjectBackend::new(Injector::new(), method),
        InjectMethod::Paste(_) => {
            let clipboard = platform_clipboard()
                .map_err(|error| anyhow!("paste-last clipboard unavailable: {error}"))?;
            EnigoInjectBackend::new(Injector::new(), method).with_clipboard(clipboard)
        }
    };
    let backend = match cancellation {
        Some(flag) => backend.with_cancellation_flag(flag),
        None => backend,
    };
    let backend = Arc::new(backend);
    if matches!(method, InjectMethod::Paste(_)) {
        register_runtime_backend(&backend);
    }
    inject_using_resolved_method(&backend, text, mode, method)
        .map_err(|error| anyhow!(error.to_string()))
}

fn shared_backend(method: InjectMethod) -> Result<Arc<EnigoInjectBackend>> {
    if matches!(method, InjectMethod::Typing) {
        return Ok(UI_TYPING_BACKEND
            .get_or_init(|| Arc::new(EnigoInjectBackend::new(Injector::new(), method)))
            .clone());
    }
    UI_BACKEND
        .get_or_init(|| {
            let clipboard = platform_clipboard()?;
            Ok(Arc::new(
                EnigoInjectBackend::new(Injector::new(), method).with_clipboard(clipboard),
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
    xkb_layout: &str,
) -> Result<()> {
    let method = resolve_method(mode, text)?;
    let backend = shared_backend(method)?;
    backend.set_target(target_title, target_process);
    backend.set_xkb_layout(xkb_layout);

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

    inject_using_resolved_method(&backend, text, mode, method)
        .map_err(|error| anyhow!(error.to_string()))
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
