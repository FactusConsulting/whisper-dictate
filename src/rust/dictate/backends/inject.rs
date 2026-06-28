//! [`InjectBackend`] impl that wraps the cross-platform [`Injector`]
//! dispatcher.
//!
//! Gated on the `rust-injection` cargo feature so default builds never
//! pull `enigo` into the dep graph. The wrapped [`Injector`] already
//! owns the platform-detection + helper-chain logic — see
//! `crate::injection::dispatcher` for the dispatch rules (enigo on
//! Windows / macOS / Linux-X11; helper chain on Linux/Wayland).
//!
//! Wave 5 PR 5-prep: no production caller in this PR — the
//! coordinator-sink wiring (PR 4) continues to use the stub injector
//! until PR 5 swaps it for this one.
//!
//! # Interior mutability
//!
//! [`InjectBackend::inject`] takes `&self` (the session keeps an
//! immutable handle to the backend across utterances), but
//! [`Injector::inject_text`] takes `&mut self` because the lazy
//! enigo-construction stash needs to mutate the trait-object slot.
//! Bridging the two requires a `Mutex` (or equivalent interior-mutability
//! primitive). `Mutex` is the right pick here because injection is a
//! coarse-grained, off-the-hot-path operation (it follows transcription
//! by definition; never contended in practice).
//!
//! Whether the wrapper itself is `Send` / `Sync` is driven by the
//! wrapped [`Injector`]'s auto-traits — `Injector` carries a
//! `Box<dyn InjectorBackend>` (no `Send` bound), so neither auto-derives
//! today. PR 5 / the supervisor can layer its own `Arc<Mutex<…>>` on
//! top if a session needs to cross threads.
//!
//! # Caller-owned pre-conditions (PR 5 wiring will own these)
//!
//! Two side-channel concerns live one layer up from this thin trait
//! impl, exactly as they do on the Python side in
//! `vp_inject.py::_inject_via_rust_backend`:
//!
//! 1. **Clipboard population for `InjectMethod::Paste`.** The wrapped
//!    [`Injector`] only sends the paste *keystroke* — it does not own
//!    the clipboard. Calling [`EnigoInjectBackend::inject`] with the
//!    transcript text while `InjectMethod::Paste(_)` is configured
//!    sends Ctrl+V (or the configured chord) against whatever the user
//!    already had on the clipboard, NOT the dictated text. PR 5's
//!    wiring must call [`crate::injection::PasteGuard::copy_with_backup`]
//!    against a real `Clipboard` implementation BEFORE invoking this
//!    backend (Python's `populate_clipboard_for_rust_paste` is the
//!    reference) and `restore()` afterwards. Codex P1 #417
//!    inject.rs:110 — this PR's scope is the trait impl only; no
//!    production [`Clipboard`](crate::injection::Clipboard) impl exists
//!    in Rust yet, so the clipboard plumbing is its own follow-up.
//!    Until that lands, [`InjectMethod::Typing`] is the
//!    production-safe choice because the dispatcher types the literal
//!    text through enigo / the helper chain regardless of clipboard
//!    state.
//! 2. **Stale-modifier release on a PTT chord.** When the push-to-talk
//!    binding includes Ctrl/Shift/Alt/Meta and the key is still down
//!    when injection starts, typed characters land as shortcuts and
//!    paste chords get extra modifiers. Python's
//!    `vp_inject._release_stale_modifiers` runs first. The Rust hotkey
//!    coordinator (`src/rust/hotkey/`) owns the press/release tracking
//!    and is the right place to issue the cleanup; PR 5's wiring will
//!    invoke it between the transcribe-completion event and this
//!    `inject()` call, matching Python's flow. Codex P2 #417
//!    inject.rs:110.

use std::sync::Mutex;

use crate::dictate::session::types::{InjectBackend, InjectError};
use crate::injection::{InjectMethod, Injector};

/// Production [`InjectBackend`] wrapping [`Injector`].
///
/// Construction is cheap — [`Injector::new`] does no system calls. The
/// underlying enigo backend (on Windows / macOS) is constructed lazily
/// inside [`Injector::inject_text`] on first use, or eagerly via
/// [`Injector::with_backend`] when a caller (e.g. a unit test) wants to
/// install a recording fake.
///
/// The chosen [`InjectMethod`] is fixed at construction; PR 5's wiring
/// will read it from the Python-side per-target config (typing /
/// paste / explicit shortcut). For "no preference" the caller passes
/// [`InjectMethod::Paste(None)`] which lets the dispatcher pick the
/// platform-appropriate shortcut at dispatch time (incl. the Linux
/// terminal-aware heuristic).
pub struct EnigoInjectBackend {
    /// `Mutex` so the trait can be implemented for `&self` even though
    /// [`Injector::inject_text`] needs `&mut self` for its lazy
    /// enigo-backend stash.
    inner: Mutex<Injector>,
    method: InjectMethod,
}

impl std::fmt::Debug for EnigoInjectBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Lock-free debug: avoid blocking on a `Mutex` we may not be
        // able to acquire (e.g. another inject in flight on a worker
        // thread when the supervisor's debug-print fires). Print the
        // outer fields only — the wrapped Injector has its own Debug
        // and the supervisor can interrogate via Self::method().
        f.debug_struct("EnigoInjectBackend")
            .field("method", &self.method)
            .field("injector", &"<Mutex<Injector>>")
            .finish()
    }
}

impl EnigoInjectBackend {
    /// Build a backend around a pre-configured [`Injector`].
    ///
    /// The caller is expected to have set the target title / process /
    /// xkb layout on the [`Injector`] via the builder methods before
    /// handing it over — those values do not change across utterances
    /// in a single session (the target is the focused window at the
    /// moment of dictation, which the supervisor refreshes between
    /// sessions).
    pub fn new(injector: Injector, method: InjectMethod) -> Self {
        Self {
            inner: Mutex::new(injector),
            method,
        }
    }

    /// The chosen injection method (typing / paste-with-shortcut).
    /// Exposed for observability / debug.
    pub fn method(&self) -> InjectMethod {
        self.method
    }
}

impl InjectBackend for EnigoInjectBackend {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        // Recover from a poisoned mutex: a panic inside a previous
        // injection (e.g. enigo failing in an unexpected way) would
        // poison the lock. The Injector itself has no internal
        // invariants that would be violated by a panic — the backend
        // trait object is the only mutable field, and `enigo::Enigo`
        // does not retain incoherent state across calls — so we
        // recover the inner value and proceed rather than wedging the
        // session forever.
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .inject_text(text, self.method)
            .map_err(|e| InjectError::Backend(format!("{e:#}")))
    }
}

#[cfg(test)]
#[path = "inject_tests.rs"]
mod tests;
