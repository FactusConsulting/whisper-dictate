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
use super::fallback::{locate_on_path, select_helper, LinuxSession};
use super::paste::PasteShortcut;
#[cfg(target_os = "linux")]
use super::wayland::{
    paste_shortcut_for, target_prefers_terminal_paste, type_text as wayland_type,
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
    backend: Option<Box<dyn InjectorBackend>>,
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

    pub fn with_xkb_layout(mut self, layout: &str) -> Self {
        layout.clone_into(&mut self.xkb_layout);
        self
    }

    /// Install a custom injection backend (a trait object). Used by tests
    /// to drive `inject_text` against a recording fake without spinning up
    /// `enigo`, and reserved for alternative backends. When unset, the
    /// dispatcher falls back to [`enigo_backend::make_default_backend`] on
    /// Windows/macOS.
    pub fn with_backend(mut self, backend: Box<dyn InjectorBackend>) -> Self {
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
    pub fn inject_text(&mut self, text: &str, method: InjectMethod) -> Result<()> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            inject_via_backend(self.backend_mut()?, text, method)
        }
        #[cfg(target_os = "linux")]
        {
            // Linux still uses the helper-chain path; the trait-object
            // backend is only consulted when a test injects one explicitly.
            if let Some(backend) = self.backend.as_deref_mut() {
                return inject_via_backend(backend, text, method);
            }
            self.inject_on_linux(text, method)
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = (text, method);
            Err(anyhow!("unsupported platform for rust injection"))
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
    /// * Linux — only through an explicitly-injected backend; the
    ///   helper-chain path is a best-effort no-op here because the
    ///   wayland.rs / ydotool route already runs `_WAYLAND_MODIFIER_RELEASES`
    ///   inside its paste-shortcut helper, so duplicating it would be
    ///   redundant noise rather than additional safety.
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
            // Helper-chain path: wayland.rs owns its own modifier-release
            // sweep before the paste shortcut. Nothing to do here.
            Ok(())
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = modifiers;
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn inject_on_linux(&self, text: &str, method: InjectMethod) -> Result<()> {
        // ydotool already has a fully-featured layout-aware code path in
        // wayland.rs — reuse it when ydotool wins the chain. The other helpers
        // get a generic invocation through super::linux_helpers.
        use super::fallback::select_paste_helper;
        use super::linux_helpers::{invoke_paste, invoke_type};

        let session = LinuxSession::detect();
        match method {
            InjectMethod::Typing => {
                let helper = select_helper(session, locate_on_path).ok_or_else(|| {
                    anyhow!(
                        "no Linux injection helper found on PATH (tried: {:?})",
                        super::fallback::fallback_chain(session)
                    )
                })?;
                if helper == "ydotool" {
                    wayland_type(text, &self.xkb_layout)
                } else {
                    invoke_type(helper, text)
                }
            }
            InjectMethod::Paste(shortcut) => {
                // P3 #371 finding 1: dotool has no paste-chord support,
                // so the paste-only helper picker filters it out.
                let helper = select_paste_helper(session, locate_on_path).ok_or_else(|| {
                    anyhow!(
                        "no Linux paste helper found on PATH (tried: {:?}, dotool excluded — no paste chord)",
                        super::fallback::fallback_chain(session)
                    )
                })?;
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
fn inject_via_backend(
    backend: &mut dyn InjectorBackend,
    text: &str,
    method: InjectMethod,
) -> Result<()> {
    match method {
        InjectMethod::Typing => backend.type_text(text),
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
            let result = injector.inject_text(&text, method);
            let response = match result {
                Ok(()) => InjectResponse {
                    ok: true,
                    error: None,
                    method: method_label(method),
                },
                Err(err) => InjectResponse {
                    ok: false,
                    error: Some(err.to_string()),
                    method: method_label(method),
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
    fn release_held_modifiers_without_backend_is_noop_on_linux() {
        // Linux helper-chain fallback: no backend installed, so the
        // dispatcher returns Ok without doing anything. The wayland.rs
        // ydotool path runs its own `_WAYLAND_MODIFIER_RELEASES` sweep
        // inside the paste-shortcut helper, so duplicating it here
        // would be redundant rather than additional safety.
        use super::super::paste::vk;
        let mut injector = Injector::new();
        assert!(injector.release_held_modifiers(&[vk::VK_CONTROL]).is_ok());
        assert!(injector.release_held_modifiers(&[]).is_ok());
    }
}
