//! Phase 2.1 high-level `Injector` API + JSON CLI handler.
//!
//! Three layers of orchestration:
//!
//! 1. [`InjectMethod`] — typed/pasted with a specific [`PasteShortcut`].
//! 2. [`Injector`] — holds platform detection (`LinuxSession`, `PasteShortcut`
//!    default) and chooses which backend to call: enigo on Win/macOS/X11, the
//!    Linux helper chain elsewhere.
//! 3. [`handle_inject`] — `whisper-dictate inject` hidden subcommand. Reads a
//!    JSON request envelope on stdin, returns a JSON response on stdout.
//!
//! The Python worker shells out via this subcommand when
//! `VOICEPI_INJECTION_BACKEND=rust` is set — see `vp_inject.py::inject_via_rust`.
//!
//! Stays small so the file is well under 500 LOC; the heavy lifting lives in
//! the focused sub-modules (`paste`, `fallback`, `enigo_backend`, `wayland`).

use std::io::{self, Read};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use super::enigo_backend::InjectorBackend;
#[cfg(target_os = "linux")]
use super::fallback::{locate_on_path, select_helper, HelperError, LinuxSession};
use super::paste::PasteShortcut;
#[cfg(target_os = "linux")]
use super::wayland::{
    paste_shortcut_for, target_prefers_terminal_paste, type_text_tracked as wayland_type_tracked,
};

/// Which strategy to use for a single injection.
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
    /// `for_linux_target` heuristic. P3 #371 finding 2: distinguishing
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
/// non-enigo backend later). This addresses P1 #2 from the PR #351 review:
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
    /// dispatcher falls back to [`enigo_backend::make_default_backend`] on
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
    ///   chain ([`fallback::select_helper`]). Today this delegates to the
    ///   existing `wayland.rs` path (`ydotool`) when `ydotool` is the pick;
    ///   the other helpers (`kwtype`, `wtype`, `dotool`, `xdotool`) get a
    ///   best-effort `Command::new(helper).args(...)` invocation.
    ///
    /// Callers that need to know whether the failure landed a partial
    /// prefix (so an outer fallback path can suppress duplicate injection
    /// -- Codex P1 #613 dispatcher.rs:599) should use
    /// [`Self::inject_text_ex`] instead.
    pub fn inject_text(&mut self, text: &str, method: InjectMethod) -> Result<()> {
        self.inject_text_ex(text, method).result
    }

    /// Same as [`Self::inject_text`] but returns [`InjectOutcome`], which
    /// carries `partial: bool` alongside the result. `partial=true` means
    /// at least one keystroke reached the compositor before the failure;
    /// an outer path (Python `_inject_via_rust_backend`, or any future
    /// caller) MUST NOT re-inject the full text because that would silently
    /// double-type the prefix. Codex P1 #613.
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
            let outcome = self.inject_on_linux(text, method);
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
            self.backend = Some(super::enigo_backend::make_default_backend()?);
        }
        Ok(self.backend.as_deref_mut().expect("just initialised"))
    }

    /// Send a bare `Release` for each VK code in `modifiers` so a stale
    /// push-to-talk chord (Ctrl / Shift / Alt / Cmd held by the user
    /// THROUGH the injection) does not turn a typed burst into shortcuts
    /// or warp a paste chord. Mirrors `vp_inject.py::_release_stale_modifiers`;
    /// called from `EnigoInjectBackend::inject` before delegating to
    /// `inject_text`. Codex P2 #417 inject.rs:110.
    ///
    /// Dispatches identically to `inject_text`:
    ///
    /// * Windows / macOS — through the active `InjectorBackend` (enigo by
    ///   default, or a test fake when one was installed via
    ///   `with_backend`).
    /// * Linux — through an explicitly-injected backend when present;
    ///   otherwise via the helper-chain release sweep (see below).
    ///
    /// # Linux helper-chain release sweep (Codex P2 #419 dispatcher.rs:184)
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
            super::linux_helpers::release_modifiers_best_effort(locate_on_path)
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = modifiers;
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn inject_on_linux(&self, text: &str, method: InjectMethod) -> InjectOutcome {
        // ydotool already has a fully-featured layout-aware code path in
        // wayland.rs — reuse it when ydotool wins the chain. The other helpers
        // get a generic invocation through super::linux_helpers.
        use super::linux_helpers::invoke_type;

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
                            // Codex P1 #665 review r3663766083 (follow-up
                            // to #657 r3663766083).
                            //
                            // Consequence: a fully failed ydotool (nothing
                            // typed) still stands the outer fallback down,
                            // so the transcript is lost in that case
                            // (#636 data-loss reopens). That's the
                            // deliberate tradeoff — double-typing into an
                            // active window is more harmful than a lost
                            // utterance the user can retry.
                            match wayland_type_tracked(text, &self.xkb_layout) {
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
                            invoke_type(helper, text).map_err(HelperError::opaque)
                        }
                    },
                    false,
                    "injection",
                )
            }
            InjectMethod::Paste(shortcut) => {
                // P3 #371 finding 1: dotool has no paste-chord support,
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
                        self.paste_with_helper(helper, shortcut)
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
    fn paste_with_helper(&self, helper: &str, shortcut: Option<PasteShortcut>) -> Result<()> {
        use super::linux_helpers::invoke_paste;
        {
            {
                if helper == "ydotool" {
                    // P2 #391 follow-up: ydotool path now also honours an
                    // explicit `Some(shortcut)`. Previously `paste_shortcut`
                    // unconditionally re-ran the terminal-target heuristic,
                    // which silently downgraded `Some(CtrlV)` to Ctrl+V on
                    // terminals (or upgraded Ctrl+Shift+V to itself on
                    // non-terminals — wrong in both directions). The new
                    // `paste_shortcut_for` falls back to the heuristic only
                    // when the caller passed `None`.
                    paste_shortcut_for(shortcut, &self.target_title, &self.target_process)
                } else {
                    // P3 #371 finding 2: only fall back to the terminal-paste
                    // heuristic when the caller did NOT pin an explicit
                    // shortcut. `Some(CtrlV)` is an explicit user choice
                    // that the heuristic must respect even though it
                    // coincides with the platform default.
                    let chosen = shortcut.unwrap_or_else(|| {
                        PasteShortcut::for_linux_target(target_prefers_terminal_paste(
                            &self.target_title,
                            &self.target_process,
                        ))
                    });
                    invoke_paste(helper, chosen)
                }
            }
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
/// [`super::enigo_backend::make_default_backend`]) and the recording fakes
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
            super::enigo_backend::send_paste_shortcut(backend, shortcut.unwrap_or_default())
        }
    }
}

// --------------------------------------------------------------------------
// JSON CLI envelope (`whisper-dictate inject`).
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InjectRequest {
    /// Inject `text` using the chosen method.
    Inject {
        text: String,
        #[serde(default)]
        method: InjectMethodSpec,
        #[serde(default)]
        target_title: String,
        #[serde(default)]
        target_process: String,
        #[serde(default)]
        xkb_layout: String,
    },
    /// Report which backend would be used (for diagnostics).
    Probe,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InjectMethodSpec {
    pub mode: InjectMode,
    /// Optional override for paste shortcuts. `"ctrl_v"`, `"ctrl_shift_v"`,
    /// `"shift_insert"`, `"cmd_v"`. Ignored for typing mode.
    #[serde(default)]
    pub shortcut: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectMode {
    Typing,
    #[default]
    Paste,
}

#[derive(Debug, Serialize)]
pub struct InjectResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub method: String,
    /// True iff at least one keystroke reached the compositor before a
    /// failure. Python's outer fallback (`vp_inject._inject`) MUST NOT
    /// re-inject the transcript when this is set -- doing so would type
    /// the successful prefix a second time on top of what already landed
    /// in the user's document. Always emitted (even on `ok: true`) so
    /// consumers can rely on the key existing. Codex P1 #613
    /// dispatcher.rs:599.
    pub partial: bool,
}

/// Result of an injection attempt with the extra `partial` signal.
///
/// `partial: true` means at least one keystroke reached the compositor
/// before the failure. Any outer fallback (Python's
/// `vp_inject._inject`, another retry loop, etc.) MUST NOT re-inject
/// the same text -- doing so would silently double-type the successful
/// prefix into the user's document. Codex P1 #613 dispatcher.rs:599.
#[derive(Debug)]
pub struct InjectOutcome {
    pub result: Result<()>,
    pub partial: bool,
}

impl InjectOutcome {
    pub fn ok() -> Self {
        Self {
            result: Ok(()),
            partial: false,
        }
    }

    pub fn failed(err: anyhow::Error) -> Self {
        Self {
            result: Err(err),
            partial: false,
        }
    }

    /// Failure that landed a partial prefix. Outer fallbacks must NOT
    /// re-run the injection or the prefix will be double-typed.
    pub fn partial(err: anyhow::Error) -> Self {
        Self {
            result: Err(err),
            partial: true,
        }
    }

    /// Bridge an existing `Result<()>` into an [`InjectOutcome`] with
    /// `partial=false`. Used by paths that cannot observe partial
    /// progress (enigo on Windows/macOS, injected trait-object backends).
    pub fn from_result(result: Result<()>) -> Self {
        match result {
            Ok(()) => Self::ok(),
            Err(err) => Self::failed(err),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProbeResponse {
    pub platform: String,
    pub linux_session: Option<String>,
    pub linux_helper: Option<String>,
    pub feature_enabled: bool,
}

/// Entry point for the `whisper-dictate inject` subcommand.
pub fn handle_inject() -> Result<()> {
    let request = read_request()?;
    match request {
        InjectRequest::Probe => {
            let body = probe_backend();
            println!("{}", serde_json::to_string(&body)?);
        }
        InjectRequest::Inject {
            text,
            method,
            target_title,
            target_process,
            xkb_layout,
        } => {
            let mut injector = Injector::new()
                .with_target(&target_title, &target_process)
                .with_xkb_layout(&xkb_layout);
            let method = resolve_method(&method)?;
            let outcome = injector.inject_text_ex(&text, method);
            let response = match outcome.result {
                Ok(()) => InjectResponse {
                    ok: true,
                    error: None,
                    method: method_label(method),
                    partial: false,
                },
                Err(err) => InjectResponse {
                    ok: false,
                    error: Some(err.to_string()),
                    method: method_label(method),
                    partial: outcome.partial,
                },
            };
            println!("{}", serde_json::to_string(&response)?);
        }
    }
    Ok(())
}

pub(crate) fn resolve_method(spec: &InjectMethodSpec) -> Result<InjectMethod> {
    Ok(match spec.mode {
        InjectMode::Typing => InjectMethod::Typing,
        InjectMode::Paste => {
            // None / empty string ⇒ "no explicit preference" so the
            // dispatcher gets to pick the platform-appropriate shortcut
            // (terminal-aware on Linux, plain default on Windows/macOS).
            // An explicit string is parsed and pinned with `Some(...)` —
            // even when the parsed value equals `PasteShortcut::default()`
            // (P3 #371 finding 2: caller-supplied default must not be
            // confused with "no preference").
            let shortcut = match spec.shortcut.as_deref() {
                None | Some("") => None,
                Some(raw) => Some(
                    PasteShortcut::parse(raw)
                        .ok_or_else(|| anyhow!("unknown paste shortcut: {raw}"))?,
                ),
            };
            InjectMethod::Paste(shortcut)
        }
    })
}

fn method_label(method: InjectMethod) -> String {
    match method {
        InjectMethod::Typing => "typing".to_owned(),
        InjectMethod::Paste(Some(s)) => format!("paste:{}", paste_label(s)),
        // `None` = no explicit shortcut; the dispatcher is free to pick
        // one at runtime. Surface that as `paste:auto` so the JSON
        // response distinguishes it from an explicit caller-pinned value.
        InjectMethod::Paste(None) => "paste:auto".to_owned(),
    }
}

fn paste_label(shortcut: PasteShortcut) -> &'static str {
    match shortcut {
        PasteShortcut::CtrlV => "ctrl_v",
        PasteShortcut::CtrlShiftV => "ctrl_shift_v",
        PasteShortcut::ShiftInsert => "shift_insert",
        PasteShortcut::CmdV => "cmd_v",
    }
}

fn probe_backend() -> ProbeResponse {
    #[cfg(target_os = "linux")]
    let (session, helper) = {
        let s = LinuxSession::detect();
        (
            Some(format!("{s:?}")),
            select_helper(s, locate_on_path).map(str::to_owned),
        )
    };
    #[cfg(not(target_os = "linux"))]
    let (session, helper) = (None, None);

    ProbeResponse {
        platform: std::env::consts::OS.to_owned(),
        linux_session: session,
        linux_helper: helper,
        feature_enabled: cfg!(feature = "rust-injection"),
    }
}

fn read_request() -> Result<InjectRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

/// Try each available helper, in chain order, until one succeeds.
///
/// Candidates come from [`available_helpers`], the same list the single-pick
/// selectors delegate to, so the runtime fallback cannot disagree with them
/// about what is eligible or in which order.
///
/// Fallback safety has two independent gates:
///
/// 1. **Progress signal** ([`HelperError::partial`]): the ydotool path
///    tracks how many evdev ops landed before the failure and marks
///    `partial: true` when any keystroke reached the compositor. That kind
///    of failure is NEVER retried -- the next helper would re-type the
///    successful prefix on top of it.
/// 2. **Text signal** ([`is_safe_to_try_next_helper`]) for subprocess
///    helpers whose partial state we can't observe from Rust. Only KNOWN
///    startup / capability signatures whitelist a retry; anything
///    unrecognised stops the chain, matching the pre-tracking behaviour.
#[cfg(target_os = "linux")]
fn try_helpers<A>(
    session: LinuxSession,
    mut attempt: A,
    paste_capable_only: bool,
    what: &str,
) -> InjectOutcome
where
    A: FnMut(&str) -> std::result::Result<(), HelperError>,
{
    let candidates =
        super::fallback::available_helpers(session, locate_on_path, paste_capable_only);
    if candidates.is_empty() {
        return InjectOutcome::failed(anyhow!(
            "no Linux {what} helper found on PATH (tried: {:?})",
            super::fallback::fallback_chain(session)
        ));
    }
    try_helpers_over(&candidates, &mut attempt, what)
}

/// Pure walk over a candidate list. Split from [`try_helpers`] so tests can
/// exercise the retry / partial-failure logic against a fake `attempt`
/// closure without touching `$PATH` or spawning subprocesses. Codex P2 #613
/// dispatcher.rs:521 -- runtime fallback needs a regression test.
#[cfg(target_os = "linux")]
fn try_helpers_over<A>(candidates: &[&str], attempt: &mut A, what: &str) -> InjectOutcome
where
    A: FnMut(&str) -> std::result::Result<(), HelperError>,
{
    if candidates.is_empty() {
        return InjectOutcome::failed(anyhow!("no Linux {what} helper available"));
    }

    let mut last_err: Option<anyhow::Error> = None;
    for (idx, helper) in candidates.iter().copied().enumerate() {
        match attempt(helper) {
            Ok(()) => {
                if idx > 0 {
                    // Worth a line: the helper the operator would expect to
                    // be used did not work, and knowing WHICH one carried
                    // the text is the difference between "it works" and
                    // "it works for a reason I can reproduce". Codex P2 #613
                    // dispatcher.rs:531 -- surface fallback diagnostics.
                    eprintln!(
                        "[inject] {what}: {helper} succeeded after {:?} failed",
                        &candidates[..idx]
                    );
                }
                return InjectOutcome::ok();
            }
            Err(HelperError {
                err, partial: true, ..
            }) => {
                // The helper had already pushed at least one keystroke
                // through the compositor when it died. Falling back to
                // the next helper would re-type the successful prefix on
                // top of it, silently corrupting the user's document.
                // Codex P1 #613 dispatcher.rs:540 -- suppress fallback on
                // any observable partial injection.
                //
                // The `partial=true` flag is propagated all the way up to
                // `InjectResponse.partial` so the Python outer fallback
                // (`vp_inject._inject`) can also stand down instead of
                // re-typing the transcript on top of the successful prefix.
                // Codex P1 #613 dispatcher.rs:599.
                eprintln!(
                    "[inject] {what}: {helper} failed AFTER typing keys ({err:#}); \
                     suppressing fallback to avoid double-typing"
                );
                return InjectOutcome::partial(err);
            }
            Err(HelperError {
                err,
                partial: false,
                known_no_progress,
            }) => {
                let text = format!("{err:#}");
                if !super::fallback::is_safe_to_try_next_helper(&text) {
                    // Unrecognised subprocess failure: it may have typed
                    // part of the text. Stop rather than risk duplicating.
                    //
                    // Codex P1 #613 dispatcher.rs:609 -- once we're past
                    // the first candidate the chain has necessarily
                    // *invoked* one or more subprocess helpers, so any
                    // later opaque failure is "possibly partial" from
                    // Python's perspective: we cannot prove nothing
                    // reached the compositor, and the outer fallback
                    // would re-type the whole transcript on top. Stamp
                    // `partial=true` in that case so the Python bridge
                    // stands down. For `idx == 0` we're back in the
                    // single-helper world the original code assumed --
                    // return without the partial stamp so the pre-#613
                    // Python fallback semantics survive verbatim.
                    //
                    // `known_no_progress` overrides the idx>0 assumption:
                    // when the current helper positively proved nothing
                    // reached the compositor (ydotool `sent == 0`), we
                    // MUST NOT stamp `partial=true`, or the Python outer
                    // fallback stands down and the transcript is lost
                    // even though we know it never landed anywhere.
                    // Codex P2 #636 dispatcher.rs:708.
                    return if idx > 0 && !known_no_progress {
                        InjectOutcome::partial(err)
                    } else {
                        InjectOutcome::failed(err)
                    };
                }
                eprintln!("[inject] {what}: {helper} unusable ({text}); trying next helper");
                last_err = Some(err);
            }
        }
    }

    InjectOutcome::failed(
        last_err
            .unwrap_or_else(|| anyhow!("no Linux {what} helper produced a result"))
            .context(format!(
                "every Linux {what} helper failed (tried: {candidates:?})"
            )),
    )
}

/// Convert a `wayland::type_text_tracked` failure into the
/// [`HelperError`] shape the fallback chain consumes. Always stamps
/// `partial: true` because the failure could carry a partial write
/// regardless of the ops-completed count.
///
/// `sent > 0` proves at least one prior evdev op succeeded — the
/// compositor already has keystrokes, so the outer fallback would
/// double-type. `sent == 0` looks like "nothing landed" but is not:
/// the single failing `ydotool type -- <buffer>` subprocess can
/// stream part of its Unicode payload through ydotoold before
/// exiting nonzero, and `run_ydotool` sees only the whole-process
/// exit status — that is indistinguishable from a spawn-time
/// refusal, so we must assume a partial write to stay safe.
///
/// The stamp matters at any candidate index. When ydotool is the
/// only installed helper (`available_helpers` places it at idx=0),
/// an `opaque` failure slips past `try_helpers_over` s `idx > 0`
/// gate and the Python outer fallback then double-types on top of
/// whatever leaked into the compositor. Split into a free function
/// so the invariant is unit-testable without a live ydotool
/// subprocess.
///
/// Codex P1 #665 review r3663766083.
#[cfg(target_os = "linux")]
fn ydotool_failure_to_helper_error(err: anyhow::Error, _sent: usize) -> HelperError {
    HelperError::partial(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_method_defaults_to_paste_with_no_explicit_shortcut() {
        // Default spec (no shortcut field) ⇒ None so the dispatcher picks
        // the platform-appropriate shortcut at runtime — including the
        // Linux terminal-aware heuristic. P3 #371 finding 2: must be
        // distinct from an explicit caller-supplied default.
        let spec = InjectMethodSpec::default();
        assert_eq!(resolve_method(&spec).unwrap(), InjectMethod::Paste(None));
    }

    #[test]
    fn resolve_method_empty_string_treated_as_no_preference() {
        let spec = InjectMethodSpec {
            mode: InjectMode::Paste,
            shortcut: Some(String::new()),
        };
        assert_eq!(resolve_method(&spec).unwrap(), InjectMethod::Paste(None));
    }

    #[test]
    fn resolve_method_typing_ignores_shortcut() {
        let spec = InjectMethodSpec {
            mode: InjectMode::Typing,
            shortcut: Some("shift_insert".to_owned()),
        };
        assert_eq!(resolve_method(&spec).unwrap(), InjectMethod::Typing);
    }

    #[test]
    fn resolve_method_honours_explicit_paste_shortcut() {
        let spec = InjectMethodSpec {
            mode: InjectMode::Paste,
            shortcut: Some("ctrl_shift_v".to_owned()),
        };
        assert_eq!(
            resolve_method(&spec).unwrap(),
            InjectMethod::Paste(Some(PasteShortcut::CtrlShiftV))
        );
    }

    #[test]
    fn resolve_method_preserves_explicit_default_value() {
        // P3 #371 finding 2 regression guard: an explicitly-supplied
        // "ctrl_v" (which happens to equal PasteShortcut::default() on
        // Linux/Windows) must NOT collapse to None — the dispatcher
        // must see Some(CtrlV) and honour it rather than running the
        // terminal-paste heuristic.
        let spec = InjectMethodSpec {
            mode: InjectMode::Paste,
            shortcut: Some("ctrl_v".to_owned()),
        };
        assert_eq!(
            resolve_method(&spec).unwrap(),
            InjectMethod::Paste(Some(PasteShortcut::CtrlV))
        );
    }

    #[test]
    fn resolve_method_rejects_unknown_shortcut() {
        let spec = InjectMethodSpec {
            mode: InjectMode::Paste,
            shortcut: Some("ctrl_alt_y".to_owned()),
        };
        assert!(resolve_method(&spec).is_err());
    }

    #[test]
    fn json_envelope_parses_inject_request() {
        let req: InjectRequest = serde_json::from_str(
            r#"{"action":"inject","text":"hi","method":{"mode":"paste","shortcut":"ctrl_v"}}"#,
        )
        .unwrap();
        match req {
            InjectRequest::Inject {
                text,
                method,
                target_title,
                target_process,
                xkb_layout,
            } => {
                assert_eq!(text, "hi");
                assert_eq!(method.mode, InjectMode::Paste);
                assert_eq!(method.shortcut.as_deref(), Some("ctrl_v"));
                assert!(target_title.is_empty());
                assert!(target_process.is_empty());
                assert!(xkb_layout.is_empty());
            }
            _ => panic!("expected Inject"),
        }
    }

    #[test]
    fn json_envelope_parses_probe_request() {
        let req: InjectRequest = serde_json::from_str(r#"{"action":"probe"}"#).unwrap();
        assert!(matches!(req, InjectRequest::Probe));
    }

    #[test]
    fn method_label_includes_paste_shortcut_name() {
        assert_eq!(
            method_label(InjectMethod::Paste(Some(PasteShortcut::CtrlShiftV))),
            "paste:ctrl_shift_v"
        );
        assert_eq!(method_label(InjectMethod::Typing), "typing");
    }

    #[test]
    fn method_label_uses_auto_for_no_explicit_shortcut() {
        // `paste:auto` distinguishes "caller did not pin a shortcut, the
        // dispatcher picked one at runtime" from an explicit caller-pinned
        // shortcut in the response JSON. P3 #371 finding 2 surface.
        assert_eq!(method_label(InjectMethod::Paste(None)), "paste:auto");
    }

    #[test]
    fn injector_builder_threads_through_state() {
        let injector = Injector::new()
            .with_target("Notepad", "notepad.exe")
            .with_xkb_layout("dk");
        assert_eq!(injector.target_title, "Notepad");
        assert_eq!(injector.target_process, "notepad.exe");
        assert_eq!(injector.xkb_layout, "dk");
    }

    // -- Trait-object backend wiring (P1 #2 from PR #351 review) --
    //
    // The dispatcher used to call `make_default_backend()` inline, so tests
    // could not exercise `inject_text` end-to-end. `with_backend()` lets us
    // plug in a recording fake on any platform — including Linux, where the
    // injected backend now wins over the helper chain.

    use std::sync::{Arc, Mutex};

    #[derive(Default, Clone)]
    struct RecordingBackend {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl super::super::enigo_backend::InjectorBackend for RecordingBackend {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.events.lock().unwrap().push(format!("type:{text}"));
            Ok(())
        }
        fn key_chord(&mut self, modifiers: &[u16], key: u16) -> Result<()> {
            let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
            self.events
                .lock()
                .unwrap()
                .push(format!("chord:[{}]+{:#x}", mods.join(","), key));
            Ok(())
        }
        fn release_modifiers(&mut self, modifiers: &[u16]) -> Result<()> {
            // Overridden so `Injector::release_held_modifiers` tests can
            // assert the modifier sweep actually reached the backend (the
            // trait's default would swallow it silently). Mirrors the
            // production enigo path that drops Ctrl / Shift / Alt / Cmd
            // before the burst lands — Codex P2 #417 inject.rs:110.
            let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
            self.events
                .lock()
                .unwrap()
                .push(format!("release:[{}]", mods.join(",")));
            Ok(())
        }
    }

    #[test]
    fn inject_text_routes_typing_through_injected_backend() {
        let backend = RecordingBackend::default();
        let events = backend.events.clone();
        let mut injector = Injector::new().with_backend(Box::new(backend));
        injector.inject_text("hello", InjectMethod::Typing).unwrap();
        assert_eq!(*events.lock().unwrap(), vec!["type:hello".to_string()]);
    }

    #[test]
    fn inject_text_routes_paste_through_injected_backend() {
        let backend = RecordingBackend::default();
        let events = backend.events.clone();
        let mut injector = Injector::new().with_backend(Box::new(backend));
        injector
            .inject_text("ignored", InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .unwrap();
        let recorded = events.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "expected single chord, got {recorded:?}");
        assert!(
            recorded[0].starts_with("chord:["),
            "expected chord event, got {:?}",
            recorded[0]
        );
    }

    #[test]
    fn inject_text_paste_with_none_uses_default_shortcut_on_backend_path() {
        // The trait-backend path (Windows/macOS) can't read the target
        // title/process so None collapses to PasteShortcut::default()
        // there. Verify a chord is still emitted rather than the call
        // panicking or no-op'ing — the Linux-specific terminal-aware
        // heuristic lives in `inject_on_linux` and is exercised separately.
        let backend = RecordingBackend::default();
        let events = backend.events.clone();
        let mut injector = Injector::new().with_backend(Box::new(backend));
        injector
            .inject_text("ignored", InjectMethod::Paste(None))
            .unwrap();
        let recorded = events.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            1,
            "expected single chord for Paste(None), got {recorded:?}"
        );
    }

    // -- release_held_modifiers wiring (Codex P2 #417 inject.rs:110) --
    //
    // The PTT-modifier sweep needs to reach the backend on Windows/macOS
    // (always) and on Linux when a backend is explicitly installed; on
    // Linux without a backend it must stay a quiet success because the
    // wayland.rs / ydotool helper already runs its own release sweep
    // before the paste chord. These tests pin all three branches.

    #[test]
    fn release_held_modifiers_forwards_to_injected_backend() {
        // Holds on Windows / macOS (always taken) AND on Linux (the
        // `if let Some(backend) = ...` early-return branch). Asserts the
        // exact VK codes propagate so a regression that drops one would
        // be caught.
        use super::super::paste::vk;
        let backend = RecordingBackend::default();
        let events = backend.events.clone();
        let mut injector = Injector::new().with_backend(Box::new(backend));
        injector
            .release_held_modifiers(&[vk::VK_CONTROL, vk::VK_SHIFT, vk::VK_MENU])
            .unwrap();
        let recorded = events.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![format!(
                "release:[{:#x},{:#x},{:#x}]",
                vk::VK_CONTROL,
                vk::VK_SHIFT,
                vk::VK_MENU,
            )]
        );
    }

    #[test]
    fn release_held_modifiers_with_empty_list_is_ok() {
        // Empty input is the "no stale modifiers held" hot path; it must
        // still reach the backend so a future implementation that, say,
        // batches into a single SendInput call sees a well-formed empty
        // request rather than the dispatcher swallowing the call.
        let backend = RecordingBackend::default();
        let events = backend.events.clone();
        let mut injector = Injector::new().with_backend(Box::new(backend));
        injector.release_held_modifiers(&[]).unwrap();
        assert_eq!(*events.lock().unwrap(), vec!["release:[]".to_string()]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn release_held_modifiers_without_backend_runs_helper_sweep_on_linux() {
        // Codex P2 #419 dispatcher.rs:184: with no trait-object backend
        // installed the call must run the helper-chain release sweep
        // (formerly a silent no-op that left wtype/kwtype/dotool paths
        // exposed to held PTT modifiers). The actual sweep shells out
        // to ydotool/xdotool when installed and silently succeeds when
        // neither is — both branches surface as `Ok(())` from the public
        // API, so we can only assert the success contract here. The
        // exact command and argument vector is pinned by the
        // `linux_helpers::plan_modifier_release` tests where the locator
        // is injectable without touching `$PATH`.
        use super::super::paste::vk;
        let mut injector = Injector::new();
        assert!(injector.release_held_modifiers(&[vk::VK_CONTROL]).is_ok());
        assert!(injector.release_held_modifiers(&[]).is_ok());
    }

    // -- Runtime fallback chain (Codex #613 findings) --------------------
    //
    // These exercise `try_helpers_over` directly with a fake `attempt`
    // closure. They pin the three behaviours that the review flagged:
    //   1. When helper A fails with a startup signature, B is tried.
    //   2. When a helper reports `HelperError::partial` (any keys landed),
    //      fallback is suppressed even if the next helper is available.
    //   3. When helper A fails first, B succeeds, an info line is emitted
    //      so the operator knows the fallback actually kicked in.

    #[cfg(target_os = "linux")]
    #[test]
    fn try_helpers_over_falls_back_after_startup_failure() {
        // Codex P2 #613 dispatcher.rs:521 -- the runtime retry loop was
        // never covered by a test. Fake: kwtype refuses at startup (a
        // known-safe signature), wtype succeeds. Expected: the second
        // helper is called and `try_helpers_over` returns Ok.
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_c = calls.clone();
        let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
            calls_c.lock().unwrap().push(helper.to_owned());
            if helper == "kwtype" {
                // "Compositor does not support the virtual keyboard
                // protocol" is a recognised startup failure per
                // `is_safe_to_try_next_helper`.
                Err(HelperError::opaque(anyhow!(
                    "kwtype type failed: Compositor does not support the virtual keyboard protocol"
                )))
            } else {
                Ok(())
            }
        };
        let candidates = ["kwtype", "wtype", "ydotool"];
        let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
        outcome
            .result
            .expect("wtype should have carried the injection");
        assert!(!outcome.partial, "success must never set partial=true");
        assert_eq!(*calls.lock().unwrap(), vec!["kwtype", "wtype"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_helpers_over_suppresses_fallback_after_partial_failure() {
        // Codex P1 #613 dispatcher.rs:540. Fake: ydotool typed some ops,
        // then died. `HelperError::partial` MUST stop the chain -- if
        // wtype ran next it would type the whole burst on top of the
        // successful prefix, silently doubling half the transcript into
        // the user's document.
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_c = calls.clone();
        let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
            calls_c.lock().unwrap().push(helper.to_owned());
            if helper == "ydotool" {
                Err(HelperError::partial(anyhow!(
                    "ydotool: broken pipe after 12 keystrokes"
                )))
            } else {
                panic!("unreachable: fallback must not run after a partial failure");
            }
        };
        let candidates = ["ydotool", "wtype"];
        let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
        assert!(
            outcome.partial,
            "partial-burst failure must stamp partial=true so the Python outer \
             fallback stands down (Codex P1 #613 dispatcher.rs:599)"
        );
        let err = outcome
            .result
            .expect_err("partial failure must NOT fall through to the next helper");
        assert!(
            format!("{err:#}").contains("broken pipe"),
            "expected the partial error surfaced verbatim, got {err:#}"
        );
        // The safety property, expressed as the call transcript.
        assert_eq!(*calls.lock().unwrap(), vec!["ydotool"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_helpers_over_stops_on_unrecognised_opaque_failure() {
        // Belt-and-braces: `HelperError::opaque` with an error string
        // that doesn't match any known startup signature must also stop
        // the chain, because a subprocess helper might have typed a
        // partial burst before it crashed with a novel message.
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_c = calls.clone();
        let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
            calls_c.lock().unwrap().push(helper.to_owned());
            Err(HelperError::opaque(anyhow!(
                "wtype type failed: killed by signal 9"
            )))
        };
        let candidates = ["wtype", "ydotool"];
        let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
        let err = outcome
            .result
            .expect_err("unrecognised failure must stop the chain");
        assert!(format!("{err:#}").contains("killed by signal 9"));
        // idx == 0 preserves the pre-#613 single-helper semantics: an
        // unrecognised opaque failure of the FIRST helper does not stamp
        // partial=true, so the Python outer fallback can still retry.
        // The `idx > 0` branch is exercised by the paired test below.
        assert!(
            !outcome.partial,
            "first-helper opaque failure must NOT stamp partial=true"
        );
        assert_eq!(*calls.lock().unwrap(), vec!["wtype"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_helpers_over_preserves_partial_false_when_helper_proves_no_progress() {
        // Codex P2 #636 dispatcher.rs:708. Fake: helper[0] (kwtype) fails
        // with a recognised startup signature -> chain retries. helper[1]
        // (ydotool) then fails with an UNrecognised opaque error but with
        // POSITIVE PROOF that nothing landed (`sent == 0`, surfaced via
        // `HelperError::none_landed`). The dispatcher must NOT stamp
        // `partial=true` in this case -- otherwise the Python bridge
        // suppresses its fallback and the transcript is lost even though
        // we know nothing was typed anywhere.
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_c = calls.clone();
        let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
            calls_c.lock().unwrap().push(helper.to_owned());
            if helper == "kwtype" {
                Err(HelperError::opaque(anyhow!(
                    "kwtype type failed: Compositor does not support the virtual keyboard protocol"
                )))
            } else {
                // ydotool: novel unrecognised message, but sent == 0 so
                // the helper KNOWS nothing reached the compositor.
                Err(HelperError::none_landed(anyhow!(
                    "ydotool type failed: broken pipe before first keystroke"
                )))
            }
        };
        let candidates = ["kwtype", "ydotool"];
        let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
        assert!(
            !outcome.partial,
            "known-no-progress opaque failure at idx>0 must NOT stamp partial=true \
             — otherwise the Python outer fallback stands down and the transcript is lost \
             (Codex P2 #636 dispatcher.rs:708)"
        );
        let err = outcome
            .result
            .expect_err("chain must still surface the underlying error");
        assert!(format!("{err:#}").contains("broken pipe"));
        assert_eq!(*calls.lock().unwrap(), vec!["kwtype", "ydotool"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_helpers_over_marks_partial_after_fallback_fired() {
        // Codex P1 #613 dispatcher.rs:609. If helper[0] fails with a
        // recognised startup signature (chain retries) and helper[1] then
        // fails with an UNrecognised opaque error, helper[1] may have
        // typed part of the transcript before dying. Stamp `partial=true`
        // so the Python outer fallback stands down instead of re-typing
        // the full text on top of the successful prefix.
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_c = calls.clone();
        let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
            calls_c.lock().unwrap().push(helper.to_owned());
            if helper == "kwtype" {
                Err(HelperError::opaque(anyhow!(
                    "kwtype type failed: Compositor does not support the virtual keyboard protocol"
                )))
            } else {
                // wtype dies with a novel unrecognised message *after*
                // some keystrokes may have landed.
                Err(HelperError::opaque(anyhow!(
                    "wtype type failed: killed by signal 9"
                )))
            }
        };
        let candidates = ["kwtype", "wtype", "ydotool"];
        let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
        assert!(
            outcome.partial,
            "opaque failure on helper idx>0 must stamp partial=true so the \
             outer Python fallback does not double-type"
        );
        let err = outcome
            .result
            .expect_err("post-fallback opaque failure must stop the chain");
        assert!(format!("{err:#}").contains("killed by signal 9"));
        // Chain did move past kwtype but stopped at wtype (no ydotool).
        assert_eq!(*calls.lock().unwrap(), vec!["kwtype", "wtype"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_helpers_over_succeeds_immediately_on_first_helper() {
        // No fallback needed: the happy path returns Ok after a single
        // call and never touches later helpers.
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_c = calls.clone();
        let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
            calls_c.lock().unwrap().push(helper.to_owned());
            Ok(())
        };
        let candidates = ["kwtype", "wtype", "ydotool"];
        let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
        outcome.result.expect("first helper succeeded, must be Ok");
        assert!(!outcome.partial, "success cannot be partial");
        assert_eq!(*calls.lock().unwrap(), vec!["kwtype"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_helpers_over_errors_when_all_helpers_fail_with_startup_signatures() {
        // Every candidate fails with a recognised startup signature: the
        // chain exhausts, and the surfaced error must be the LAST one
        // (wrapped with a "every ... helper failed" context).
        let mut attempt = |helper: &str| -> std::result::Result<(), HelperError> {
            Err(HelperError::opaque(anyhow!(
                "{helper} type failed: Compositor does not support the virtual keyboard protocol"
            )))
        };
        let candidates = ["kwtype", "wtype"];
        let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
        let err = outcome
            .result
            .expect_err("chain of startup failures should surface an error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("every Linux injection helper failed"),
            "got: {msg}"
        );
        // Every failure was a recognised startup signature (i.e. no
        // helper is believed to have typed anything), so `partial=false`.
        assert!(
            !outcome.partial,
            "recognised startup-only failures cannot have typed a partial burst"
        );
    }

    // -----------------------------------------------------------------------
    // Codex P1 #665 review r3663766083 — ydotool failure ALWAYS stamps
    // `partial=true`. The regression this pins:
    //
    //   1. `available_helpers` places `ydotool` at candidate index 0 when
    //      it is the only installed helper.
    //   2. A `ydotool type -- <buffer>` subprocess emits part of the
    //      buffer, then exits nonzero (broken ydotoold socket, SIGPIPE,
    //      etc.). `type_text_tracked` returns `Err(err, 0)` — no OP
    //      completed, so `sent == 0`.
    //   3. The previous `HelperError::opaque(err)` reached
    //      `try_helpers_over` at idx=0, whose `idx > 0` gate skipped the
    //      partial stamp, so `outcome.partial == false`.
    //   4. Python outer fallback saw `partial == false` and retried the
    //      whole transcript on top of the leaked prefix → double-typing.
    //
    // Failure mode this test would exhibit against the un-fixed code:
    // `assert!(outcome.partial, ...)` FAILS because the un-fixed code
    // returns `HelperError::opaque(err)` at idx=0, and `try_helpers_over`
    // leaves `partial=false`.
    // -----------------------------------------------------------------------

    #[cfg(target_os = "linux")]
    #[test]
    fn ydotool_failure_conversion_always_stamps_partial_regardless_of_sent() {
        // The direct invariant on the extracted helper — locks the
        // conversion so any future edit that flips this back to
        // `opaque` fails a test before it can ship.
        for sent in [0usize, 1, 2, 47] {
            let err = anyhow!("ydotool type failed: broken pipe after chunk {sent}");
            let helper_err = ydotool_failure_to_helper_error(err, sent);
            assert!(
                helper_err.partial,
                "ydotool failure at sent={sent} must set partial=true; \
                 a partial-write can happen at any sent count (Codex P1 #665 \
                 review r3663766083)",
            );
            assert!(
                !helper_err.known_no_progress,
                "ydotool subprocess exit code cannot prove zero events landed; \
                 `known_no_progress` must stay clear",
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_helpers_over_stamps_partial_when_ydotool_is_only_helper_and_fails() {
        // End-to-end for the exact scenario Codex flagged: ydotool is the
        // ONLY installed helper (idx=0), its subprocess fails with an
        // unrecognised message, and the outcome must carry partial=true
        // so the Python outer fallback stands down.
        //
        // Uses `ydotool_failure_to_helper_error` (the production wiring
        // in `inject_on_linux`) so this test would ALSO fail against a
        // regression that reverted just that call site while leaving the
        // helper function intact.
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_c = calls.clone();
        let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
            calls_c.lock().unwrap().push(helper.to_owned());
            assert_eq!(helper, "ydotool", "test fixture only provides ydotool");
            Err(ydotool_failure_to_helper_error(
                anyhow!("ydotool type failed: broken pipe mid-buffer"),
                0, // first op failed — the exact case that slipped past idx>0
            ))
        };
        let candidates = ["ydotool"];
        let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
        let err = outcome
            .result
            .expect_err("sole-helper failure must surface an error");
        assert!(
            format!("{err:#}").contains("broken pipe"),
            "surfaced error must carry the original ydotool message, got: {err:#}",
        );
        assert!(
            outcome.partial,
            "ydotool-only failure at idx=0 must stamp partial=true so the Python \
             outer fallback does not double-type on top of a partial ydotool write \
             — Codex P1 #665 review r3663766083",
        );
        assert_eq!(*calls.lock().unwrap(), vec!["ydotool"]);
    }
}
