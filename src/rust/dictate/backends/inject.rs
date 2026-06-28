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
//! # Pre-injection cleanup (Codex P1 / P2 #417 + #419)
//!
//! Calling `Injector::inject_text` directly leaves two gaps that the
//! Python path (`vp_inject.py::_inject_via_rust_backend`) closes before
//! delegating. This wrapper closes both inside `inject()` so a Rust
//! `DictateSession` (PR 5) can swap the stub backend for this one
//! without a separate caller-side dance:
//!
//! 1. **Clipboard ownership for paste mode.** The dispatcher's
//!    `Paste(_)` arm only sends the keystroke; its module doc explicitly
//!    states the caller is responsible for populating the clipboard
//!    first. A Rust-native session hands this wrapper the transcript
//!    itself, so without owning the copy/restore step we would silently
//!    paste whatever the user already had on their clipboard. The
//!    wrapper accepts an optional [`Clipboard`] impl via
//!    [`Self::with_clipboard`]; on `Paste(_)` it stashes the previous
//!    contents, writes the transcript, sends the chord, **waits for the
//!    paste-read window to elapse** (matching Python's
//!    `_CLIPBOARD_RESTORE_DELAY_S = 2.0` — Wayland's `wl-copy` and
//!    slower GUI apps read the clipboard lazily so restoring instantly
//!    races the paste itself, Codex P1 #419 inject.rs:266), then
//!    restores the previous value via [`PasteGuard`]. The delay is
//!    parametrisable through [`Self::with_restore_delay`] so unit tests
//!    can pass [`Duration::ZERO`]. Paste-mode injection without a
//!    configured clipboard returns [`InjectError::Backend`] rather than
//!    silently pasting stale data — surfacing the misconfiguration
//!    loudly instead of mangling a transcript.
//! 2. **Stale push-to-talk modifiers.** A modifier PTT (Shift / Ctrl /
//!    Alt / Cmd) is held physically THROUGH the dictation: when the
//!    inject burst lands, the OS still sees the modifier down, so a
//!    typing burst becomes `Ctrl+<char>` shortcuts and a paste chord
//!    gets warped. The wrapper calls
//!    [`Injector::release_held_modifiers`] before delegating, mirroring
//!    the Python `_release_stale_modifiers` sweep over the full
//!    Shift / Alt / Ctrl / Cmd set.
//!
//! Both steps fail-soft (log + continue) when reasonable, matching the
//! Python path's permissive philosophy: a missing modifier release is
//! strictly less bad than failing the inject entirely and dropping the
//! transcript. The clipboard write is the exception — there a failure
//! means we'd paste stale data, so we abort BEFORE sending the chord.
//!
//! # Interior mutability
//!
//! [`InjectBackend::inject`] takes `&self` (the session keeps an
//! immutable handle to the backend across utterances), but
//! [`Injector::inject_text`] takes `&mut self` because the lazy
//! enigo-construction stash needs to mutate the trait-object slot. The
//! optional [`Clipboard`] backend also needs `&mut self` (read / write
//! are mutating on every real impl). Bridging the two requires a
//! `Mutex` (or equivalent interior-mutability primitive); we use a
//! single `Mutex` over a small inner struct so a paste injection can
//! drive both the injector and the clipboard inside the same critical
//! section. Injection is coarse-grained and off-the-hot-path (it
//! follows transcription by definition; never contended in practice),
//! so the lock is cheap.
//!
//! Whether the wrapper itself is `Send` / `Sync` is driven by the
//! wrapped [`Injector`]'s auto-traits — `Injector` carries a
//! `Box<dyn InjectorBackend>` (no `Send` bound), so neither auto-derives
//! today. PR 5 / the supervisor can layer its own `Arc<Mutex<…>>` on
//! top if a session needs to cross threads.

use std::sync::Mutex;
use std::time::Duration;

use crate::dictate::session::types::{InjectBackend, InjectError};
use crate::injection::paste::{vk, Clipboard, PasteGuard};
use crate::injection::{InjectMethod, Injector};

/// VKs the wrapper releases before every injection, matching the full
/// Shift / Alt / Ctrl / Cmd sweep in `vp_inject.py::_release_stale_modifiers`.
///
/// Side-specific variants (`VK_R*`) are listed alongside the generic VKs
/// because Win32 distinguishes the two at the keyboard layer: a PTT
/// binding like `ctrl_r` or `shift_r+ctrl_r` leaves the right-side
/// scancode logically down and the generic `VK_CONTROL`/`VK_SHIFT`
/// release does NOT clear it. Mirrors Python's `_release_stale_modifiers`
/// loop over `ctrl`/`ctrl_l`/`ctrl_r` (etc.) via pynput. Codex P2 #419
/// inject.rs:84.
///
/// Order is fixed and asserted on by the unit tests so an accidental
/// reorder is caught even when the resulting behaviour would be
/// identical at runtime.
pub(crate) const STALE_MODIFIER_VKS: &[u16] = &[
    vk::VK_SHIFT,
    vk::VK_RSHIFT,
    vk::VK_MENU,
    vk::VK_RMENU,
    vk::VK_CONTROL,
    vk::VK_RCONTROL,
    vk::VK_LWIN,
    vk::VK_RWIN,
];

/// How long to wait between sending the paste chord and restoring the
/// previous clipboard contents. Mirrors `_CLIPBOARD_RESTORE_DELAY_S = 2.0`
/// in `vp_inject.py`. Paste targets (especially Wayland's `wl-copy` which
/// serves clipboard content at request time, and slower GUI apps) may
/// read the clipboard lazily / asynchronously — restoring instantly
/// races against the very paste we just triggered and the target ends up
/// with the user's previous clipboard contents instead of the dictated
/// text. Codex P1 #419 inject.rs:266.
pub(crate) const DEFAULT_CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(2000);

/// Interior state guarded by a single `Mutex`. Keeping the injector and
/// the clipboard under the same lock lets a `Paste(_)` injection drive
/// both atomically: the clipboard write must happen between the
/// modifier release and the paste chord, with no interleaving from a
/// parallel `inject()` call on the same backend.
struct State {
    injector: Injector,
    /// Caller-supplied clipboard backend. Required for paste-mode
    /// injection; left `None` for typing-only wrappers (and for the
    /// tests that exercise the typing path). Boxed-dyn rather than a
    /// generic parameter so the wrapper can sit behind
    /// `Box<dyn InjectBackend>` in the dictate-session sink without
    /// leaking a phantom type parameter.
    clipboard: Option<Box<dyn Clipboard>>,
}

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
///
/// **Paste mode requires a [`Clipboard`] backend.** Pair the wrapper
/// with [`Self::with_clipboard`] before handing it to any code path
/// that may call [`Self::inject`] under a paste method; otherwise the
/// paste arm surfaces [`InjectError::Backend`] rather than silently
/// sending the chord against stale clipboard data.
pub struct EnigoInjectBackend {
    /// `Mutex` over the combined injector + clipboard state so the
    /// trait can be implemented for `&self` even though both inner
    /// pieces need exclusive access during a single `inject` call.
    inner: Mutex<State>,
    method: InjectMethod,
    /// Delay between sending the paste chord and restoring the previous
    /// clipboard contents — see [`DEFAULT_CLIPBOARD_RESTORE_DELAY`] for
    /// the rationale (Codex P1 #419 inject.rs:266). Tests override to
    /// `Duration::ZERO` via [`Self::with_restore_delay`] so they don't
    /// pay the 2 s wall-clock wait per paste assertion.
    restore_delay: Duration,
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
            .field("restore_delay", &self.restore_delay)
            .field("inner", &"<Mutex<State>>")
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
    ///
    /// No clipboard is wired by default — pair with
    /// [`Self::with_clipboard`] for paste mode. Typing-only wrappers
    /// can skip the clipboard step.
    pub fn new(injector: Injector, method: InjectMethod) -> Self {
        Self {
            inner: Mutex::new(State {
                injector,
                clipboard: None,
            }),
            method,
            restore_delay: DEFAULT_CLIPBOARD_RESTORE_DELAY,
        }
    }

    /// Install a [`Clipboard`] backend used by paste mode to save the
    /// previous contents, write the transcript, and restore the
    /// previous value after the paste chord fires (via [`PasteGuard`]).
    /// Required for [`InjectMethod::Paste`] — typing mode ignores it
    /// entirely so a typing-only wrapper can skip this step.
    pub fn with_clipboard(mut self, clipboard: Box<dyn Clipboard>) -> Self {
        // `get_mut` skips the lock entirely; safe because `self` is owned
        // here so no other reference to the Mutex can exist.
        self.inner
            .get_mut()
            .expect("freshly-constructed mutex is uncontended")
            .clipboard = Some(clipboard);
        self
    }

    /// Override the post-chord clipboard-restore delay. Production uses
    /// [`DEFAULT_CLIPBOARD_RESTORE_DELAY`] (2 s, parity with Python's
    /// `_CLIPBOARD_RESTORE_DELAY_S`); tests pass [`Duration::ZERO`] to
    /// skip the wall-clock wait. Codex P1 #419 inject.rs:266.
    ///
    /// Set on the wrapper rather than on `Injector` because the delay is
    /// a property of the *wrapping* paste-guard semantics (which only
    /// this layer owns) — `Injector::inject_text` doesn't know that a
    /// guard is in flight.
    pub fn with_restore_delay(mut self, delay: Duration) -> Self {
        self.restore_delay = delay;
        self
    }

    /// The chosen injection method (typing / paste-with-shortcut).
    /// Exposed for observability / debug.
    pub fn method(&self) -> InjectMethod {
        self.method
    }

    /// The configured clipboard-restore delay. Exposed primarily for
    /// tests asserting the default; production callers should not need
    /// to read this back.
    pub fn restore_delay(&self) -> Duration {
        self.restore_delay
    }
}

impl InjectBackend for EnigoInjectBackend {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        // Recover from a poisoned mutex: a panic inside a previous
        // injection (e.g. enigo failing in an unexpected way) would
        // poison the lock. Neither the injector nor the clipboard
        // carry invariants violated by a panic — `enigo::Enigo` does
        // not retain incoherent state across calls and the
        // `Clipboard` trait is stateless apart from the backing OS
        // surface — so we recover the inner value and proceed rather
        // than wedging the session forever.
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = &mut *guard;

        // Pre-injection cleanup #1: drop any modifiers still held from
        // a push-to-talk chord. Failures are logged + ignored to match
        // the Python `_release_stale_modifiers` permissive behaviour;
        // losing a release would land the burst as shortcuts but
        // failing the inject would lose the transcript outright.
        // Codex P2 #417 inject.rs:110.
        if let Err(e) = state.injector.release_held_modifiers(STALE_MODIFIER_VKS) {
            eprintln!("[inject] stale-modifier release failed: {e:#}");
        }

        match self.method {
            InjectMethod::Typing => state
                .injector
                .inject_text(text, InjectMethod::Typing)
                .map_err(|e| InjectError::Backend(format!("{e:#}"))),
            InjectMethod::Paste(_) => {
                inject_via_paste(state, text, self.method, self.restore_delay)
            }
        }
    }
}

/// Paste-mode injection arm: own the clipboard copy/restore so the
/// dispatcher's "Python-already-copied" assumption holds even when the
/// caller is a Rust-native `DictateSession`. Codex P1 #417 inject.rs:110
/// + Codex P1 #419 inject.rs:266 (restore-delay parity with Python).
///
/// Pulled into a free function so the borrow story stays obvious — the
/// caller destructures `state` once and the function owns the disjoint
/// `&mut Injector` + `&mut dyn Clipboard` borrows from there.
fn inject_via_paste(
    state: &mut State,
    text: &str,
    method: InjectMethod,
    restore_delay: Duration,
) -> Result<(), InjectError> {
    let clipboard = state.clipboard.as_deref_mut().ok_or_else(|| {
        InjectError::Backend(
            "paste injection requires a clipboard backend; call \
             EnigoInjectBackend::with_clipboard before using Paste(_)"
                .to_owned(),
        )
    })?;

    // Save the previous clipboard + write the transcript. If the write
    // fails we abort BEFORE sending the chord — pasting whatever was
    // already on the clipboard would silently inject the wrong text.
    let paste_guard = PasteGuard::copy_with_backup(clipboard, text).ok_or_else(|| {
        InjectError::Backend(
            "clipboard write failed; refusing to send paste shortcut \
             against stale clipboard contents"
                .to_owned(),
        )
    })?;

    let inject_result = state
        .injector
        .inject_text(text, method)
        .map_err(|e| InjectError::Backend(format!("{e:#}")));

    // Hold the paste guard alive until the target has had time to read
    // the clipboard. Wayland / wl-copy serves contents at request time
    // and slower GUI apps poll the clipboard async — restoring instantly
    // races the paste itself and the target ends up with the user's
    // previous clipboard contents instead of the dictated text. Mirrors
    // Python's `_restore_clipboard_after_delay` (`time.sleep(2.0)` on
    // its daemon thread). Skipped when the caller pinned
    // `Duration::ZERO` — production keeps the 2 s default, tests pass
    // ZERO. Codex P1 #419 inject.rs:266.
    if !restore_delay.is_zero() {
        std::thread::sleep(restore_delay);
    }

    // Restore the previous clipboard whether or not the chord went
    // through — the user's prior clipboard contents are sacred. The
    // restore is gated on the clipboard still holding OUR text so a
    // user mid-paste copy is never clobbered; that check lives in
    // `PasteGuard::restore` itself.
    paste_guard.restore(clipboard);

    inject_result
}

// Test modules live in sibling files (the `inject.rs` parent is a
// file-form module, so without `#[path]` Rust would look for
// `dictate/backends/inject/<name>.rs` — keeping the tests next to
// their target file is more readable).
#[cfg(test)]
#[path = "inject_cleanup_tests.rs"]
mod inject_cleanup_tests;
#[cfg(test)]
#[path = "inject_test_support.rs"]
mod inject_test_support;
#[cfg(test)]
#[path = "inject_tests.rs"]
mod inject_tests;
