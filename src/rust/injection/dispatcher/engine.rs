//! Platform dispatch and lazy ownership of the native keyboard backend.

#[cfg(target_os = "linux")]
use super::chain::{try_helpers, ydotool_failure_to_helper_error};
use super::outcome::InjectOutcome;
use crate::injection::enigo_backend::InjectorBackend;
#[cfg(target_os = "linux")]
use crate::injection::fallback::{locate_on_path, HelperError, LinuxSession};
use crate::injection::paste::PasteShortcut;
#[cfg(target_os = "linux")]
use crate::injection::wayland::{
    paste_shortcut_for_cancellable, target_prefers_terminal_paste,
    type_text_tracked_cancellable as wayland_type_tracked_cancellable,
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectMethod {
    /// Direct key-event injection. Slow but reliable for plain text.
    Typing,
    /// Copy to clipboard, send paste keystroke, restore previous clipboard.
    ///
    /// `Some(shortcut)` is an EXPLICIT user choice — the dispatcher must
    /// honour it even when the value coincidentally matches the platform
    /// default. `None` means "no preference, pick the platform-appropriate
    /// shortcut at dispatch time" — on Linux that means the terminal-aware
    /// `for_linux_target` heuristic: distinguishing
    /// explicit-equals-default from "no preference" is impossible if the
    /// caller has to express both as a bare `PasteShortcut` value.
    Paste(Option<PasteShortcut>),
}

impl Default for InjectMethod {
    fn default() -> Self {
        InjectMethod::Paste(None)
    }
}

/// High-level entry point. Construction is cheap — no system calls, no
/// helper-binary lookups. The actual injection happens in [`Injector::inject_text`].
///
/// The enigo backend is constructed lazily on the first Windows/macOS
/// injection, BUT may be pre-supplied via [`Injector::with_backend`] so unit
/// tests can plug in a recording fake (and to keep the door open for a
/// non-enigo backend later). This addresses from the PR #351 review:
/// the dispatcher no longer hard-codes the enigo path.
pub struct Injector {
    target_title: String,
    target_process: String,
    xkb_layout: String,
    backend: Option<Box<dyn InjectorBackend + Send>>,
}

impl std::fmt::Debug for Injector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Injector")
            .field("target_title", &self.target_title)
            .field("target_process", &self.target_process)
            .field("xkb_layout", &self.xkb_layout)
            .field(
                "backend",
                &self.backend.as_ref().map(|_| "<dyn InjectorBackend>"),
            )
            .finish()
    }
}

impl Injector {
    pub fn new() -> Self {
        Injector {
            target_title: String::new(),
            target_process: String::new(),
            xkb_layout: String::new(),
            backend: None,
        }
    }

    pub fn with_target(mut self, title: &str, process: &str) -> Self {
        title.clone_into(&mut self.target_title);
        process.clone_into(&mut self.target_process);
        self
    }

    pub fn set_target(&mut self, title: &str, process: &str) {
        title.clone_into(&mut self.target_title);
        process.clone_into(&mut self.target_process);
    }

    pub fn set_xkb_layout(&mut self, layout: &str) {
        layout.clone_into(&mut self.xkb_layout);
    }

    pub fn with_xkb_layout(mut self, layout: &str) -> Self {
        layout.clone_into(&mut self.xkb_layout);
        self
    }

    /// Install a custom injection backend (a trait object). Used by tests
    /// to drive `inject_text` against a recording fake without spinning up
    /// `enigo`, and reserved for alternative backends. When unset, the
    /// dispatcher falls back to [`crate::injection::enigo_backend::make_default_backend`] on
    /// Windows/macOS.
    pub fn with_backend(mut self, backend: Box<dyn InjectorBackend + Send>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Run an injection. Dispatch:
    ///
    /// * **Windows / macOS** — always go through the injected backend, or
    ///   construct enigo on demand (requires the `rust-injection` feature,
    ///   since `enigo` is an optional dep).
    /// * **Linux** — choose the first available helper in the per-session
    ///   chain ([`crate::injection::fallback::select_helper`]). Today this delegates to the
    ///   existing `wayland.rs` path (`ydotool`) when `ydotool` is the pick;
    ///   the other helpers (`kwtype`, `wtype`, `dotool`, `xdotool`) get a
    ///   best-effort `Command::new(helper).args(...)` invocation.
    ///
    /// Callers that need to know whether the failure landed a partial
    /// prefix (so an outer fallback path can suppress duplicate injection
    /// -- dispatcher.rs:599) should use
    /// [`Self::inject_text_ex`] instead.
    pub fn inject_text(&mut self, text: &str, method: InjectMethod) -> Result<()> {
        self.inject_text_ex(text, method).result
    }

    /// Same as [`Self::inject_text`] but returns [`InjectOutcome`], which
    /// carries `partial: bool` alongside the result. `partial=true` means
    /// at least one keystroke reached the compositor before the failure;
    /// an outer path (Python `_inject_via_rust_backend`, or any future
    /// caller) MUST NOT re-inject the full text because that would silently
    /// double-type the prefix.
    pub fn inject_text_ex(&mut self, text: &str, method: InjectMethod) -> InjectOutcome {
        self.inject_text_cancellable(text, method, &|| true)
    }

    /// Run an injection while checking `should_continue` between characters
    /// on the native keyboard path. This lets runtime teardown stop a long
    /// typing burst after the current key instead of allowing the whole text
    /// to land after the UI reports Stop.
    pub fn inject_text_cancellable(
        &mut self,
        text: &str,
        method: InjectMethod,
        should_continue: &dyn Fn() -> bool,
    ) -> InjectOutcome {
        if !should_continue() {
            return InjectOutcome::failed(anyhow!("injection cancelled"));
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let backend = match self.backend_mut() {
                Ok(b) => b,
                Err(err) => return InjectOutcome::failed(err),
            };
            InjectOutcome::from_result(inject_via_backend_cancellable(
                backend,
                text,
                method,
                should_continue,
            ))
        }
        #[cfg(target_os = "linux")]
        {
            // Linux still uses the helper-chain path; the trait-object
            // backend is only consulted when a test injects one explicitly.
            if let Some(backend) = self.backend.as_deref_mut() {
                return InjectOutcome::from_result(inject_via_backend_cancellable(
                    backend,
                    text,
                    method,
                    should_continue,
                ));
            }
            let outcome = self.inject_on_linux_cancellable(text, method, should_continue);
            if outcome.result.is_ok() && !should_continue() {
                InjectOutcome::failed(anyhow!("injection cancelled"))
            } else {
                outcome
            }
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = (text, method);
            InjectOutcome::failed(anyhow!("unsupported platform for rust injection"))
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn backend_mut(&mut self) -> Result<&mut dyn InjectorBackend> {
        if self.backend.is_none() {
            self.backend = Some(crate::injection::enigo_backend::make_default_backend()?);
        }
        Ok(self.backend.as_deref_mut().expect("just initialised"))
    }

    /// Send a bare `Release` for each VK code in `modifiers` so a stale
    /// push-to-talk chord (Ctrl / Shift / Alt / Cmd held by the user
    /// THROUGH the injection) does not turn a typed burst into shortcuts
    /// or warp a paste chord. Mirrors `vp_inject.py::_release_stale_modifiers`;
    /// called from `EnigoInjectBackend::inject` before delegating to
    /// `inject_text`. inject.rs:110.
    ///
    /// Dispatches identically to `inject_text`:
    ///
    /// * Windows / macOS — through the active `InjectorBackend` (enigo by
    ///   default, or a test fake when one was installed via
    ///   `with_backend`).
    /// * Linux — through an explicitly-injected backend when present;
    ///   otherwise via the helper-chain release sweep (see below).
    ///
    /// # Linux helper-chain release sweep (dispatcher.rs:184)
    ///
    /// `inject_on_linux` may select `kwtype` / `wtype` (Wayland) or
    /// `dotool` for the actual inject, and **none of those have an
    /// equivalent of `xdotool --clearmodifiers`** — a PTT modifier held
    /// through dictation therefore corrupts the burst on those paths.
    /// The previous behaviour returned `Ok(())` without doing anything,
    /// claiming `wayland.rs` owned the release; but `wayland.rs` only
    /// runs that prelude when ydotool wins the chain, so the gap was
    /// real. We now best-effort release via `ydotool` (Wayland evdev
    /// key-ups) or `xdotool` (X11 `keyup` verb) — regardless of which
    /// helper the upcoming inject will use — so the modifier is dropped
    /// before the inject lands. Hosts with only wtype/kwtype/dotool
    /// installed get a silent `Ok`, matching the existing
    /// failure-permissive philosophy.
    pub fn release_held_modifiers(&mut self, modifiers: &[u16]) -> Result<()> {
        self.release_held_modifiers_cancellable(modifiers, &|| true)
    }

    pub fn release_held_modifiers_cancellable(
        &mut self,
        modifiers: &[u16],
        should_continue: &dyn Fn() -> bool,
    ) -> Result<()> {
        if !should_continue() {
            return Err(anyhow!("injection cancelled"));
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.backend_mut()?.release_modifiers(modifiers)
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(backend) = self.backend.as_deref_mut() {
                return backend.release_modifiers(modifiers);
            }
            // Helper-chain path: best-effort release via ydotool/xdotool.
            // `modifiers` is informational (the helper sweep is helper-
            // specific, not VK-keyed) — we drain the full common set
            // either way, matching the all-or-nothing semantics of
            // `xdotool --clearmodifiers` and `WAYLAND_MODIFIER_RELEASES`.
            let _ = modifiers;
            crate::injection::linux_helpers::release_modifiers_best_effort_cancellable(
                locate_on_path,
                should_continue,
            )
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = modifiers;
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn inject_on_linux_cancellable(
        &self,
        text: &str,
        method: InjectMethod,
        should_continue: &dyn Fn() -> bool,
    ) -> InjectOutcome {
        // ydotool already has a fully-featured layout-aware code path in
        // wayland.rs — reuse it when ydotool wins the chain. The other helpers
        // get a generic invocation through crate::injection::linux_helpers.
        use crate::injection::linux_helpers::invoke_type_cancellable;

        let session = LinuxSession::detect();
        match method {
            InjectMethod::Typing => {
                // Walk the chain instead of committing to the first helper
                // that merely EXISTS on PATH. Presence is not capability:
                // on KDE Wayland `wtype` is installed and shipped by the
                // distro, but KWin does not implement
                // `zwp_virtual_keyboard_v1`, so every injection failed while
                // a working `ydotool` sat two entries further down and was
                // never tried.
                try_helpers(
                    session,
                    |helper| {
                        if helper == "ydotool" {
                            // ydotool goes through the evdev path which
                            // splits into multiple sub-invocations. On any
                            // failure we CANNOT prove nothing landed in the
                            // compositor: even at `sent == 0` (the first op
                            // failed), the single `ydotool type -- <buffer>`
                            // subprocess may have already streamed part of
                            // its Unicode buffer through ydotoold before
                            // exiting nonzero — `run_ydotool` sees only the
                            // whole-process exit status, so a partial write
                            // is indistinguishable from a socket-refused
                            // spawn. Stamp `partial(err)` directly (not
                            // `opaque`) so the outcome carries the
                            // partial-write signal regardless of the
                            // helper's index inside `try_helpers_over`:
                            // when ydotool is the ONLY installed helper
                            // (`available_helpers` places it at idx=0), an
                            // `opaque` failure would slip past the `idx > 0`
                            // stamp and let the Python outer fallback
                            // double-type on top of whatever leaked. See
                            // review r3663766083 (follow-up
                            // to #657 r3663766083).
                            //
                            // Consequence: a fully failed ydotool (nothing
                            // typed) still stands the outer fallback down,
                            // so the transcript is lost in that case
                            // (#636 data-loss reopens). That's the
                            // deliberate tradeoff — double-typing into an
                            // active window is more harmful than a lost
                            // utterance the user can retry.
                            match wayland_type_tracked_cancellable(
                                text,
                                &self.xkb_layout,
                                should_continue,
                            ) {
                                Ok(_) => Ok(()),
                                Err((err, sent)) => Err(ydotool_failure_to_helper_error(err, sent)),
                            }
                        } else {
                            // kwtype / wtype / dotool / xdotool: single
                            // opaque subprocess. We can't observe partial
                            // progress; fall back to the text-based
                            // `is_safe_to_try_next_helper` gate in
                            // try_helpers -- it only whitelists KNOWN
                            // startup / capability signatures, so any
                            // unrecognised error stops the chain.
                            invoke_type_cancellable(helper, text, should_continue)
                                .map_err(HelperError::opaque)
                        }
                    },
                    false,
                    "injection",
                )
            }
            InjectMethod::Paste(shortcut) => {
                // dotool has no paste-chord support,
                // so the paste-only helper picker filters it out.
                //
                // Paste is a single chord: either the whole thing lands
                // or nothing does. `HelperError::opaque` is correct -- a
                // mid-chord failure wouldn't type visible characters into
                // the document anyway (the modifier stays down / comes
                // back up with no printable payload).
                try_helpers(
                    session,
                    |helper| {
                        self.paste_with_helper_cancellable(helper, shortcut, should_continue)
                            .map_err(HelperError::opaque)
                    },
                    true,
                    "paste",
                )
            }
        }
    }

    /// One paste attempt with a chosen helper. Split out of the chain walk so
    /// the per-helper shortcut logic below stays readable and the retry loop
    /// stays about retrying.
    #[cfg(target_os = "linux")]
    fn paste_with_helper_cancellable(
        &self,
        helper: &str,
        shortcut: Option<PasteShortcut>,
        should_continue: &dyn Fn() -> bool,
    ) -> Result<()> {
        use crate::injection::linux_helpers::invoke_paste_cancellable;
        if helper == "ydotool" {
            paste_shortcut_for_cancellable(
                shortcut,
                &self.target_title,
                &self.target_process,
                should_continue,
            )
        } else {
            let chosen = shortcut.unwrap_or_else(|| {
                PasteShortcut::for_linux_target(target_prefers_terminal_paste(
                    &self.target_title,
                    &self.target_process,
                ))
            });
            invoke_paste_cancellable(helper, chosen, should_continue)
        }
    }
}

impl Default for Injector {
    fn default() -> Self {
        Self::new()
    }
}

/// Drive an arbitrary [`InjectorBackend`] trait object — the same code path
/// runs both the production enigo backend (made via
/// [`crate::injection::enigo_backend::make_default_backend`]) and the recording fakes
/// in `dispatcher::tests`. Available on every platform so a test-supplied
/// backend works on Linux too.
fn inject_via_backend_cancellable(
    backend: &mut dyn InjectorBackend,
    text: &str,
    method: InjectMethod,
    should_continue: &dyn Fn() -> bool,
) -> Result<()> {
    if !should_continue() {
        return Err(anyhow!("injection cancelled"));
    }
    match method {
        InjectMethod::Typing => backend.type_text_cancellable(text, should_continue),
        InjectMethod::Paste(shortcut) => {
            // The dispatcher doesn't own the clipboard here; the Python
            // worker populates it (see `vp_inject._inject_via_rust_backend`)
            // and merely asks us to send the keystroke. Rust-side clipboard
            // ownership is wired by the PasteGuard in paste.rs and is
            // exercised by unit tests; this arm avoids double-copy when
            // Python already populated the clipboard via the existing
            // _paste() path.
            //
            // `None` (no explicit shortcut) collapses to `PasteShortcut::default()`
            // for the enigo-backed Windows/macOS path — the Linux terminal-paste
            // heuristic lives in `inject_on_linux`, which is the only path
            // that can read the target title/process.
            if !should_continue() {
                return Err(anyhow!("injection cancelled"));
            }
            crate::injection::enigo_backend::send_paste_shortcut(
                backend,
                shortcut.unwrap_or_default(),
            )
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
