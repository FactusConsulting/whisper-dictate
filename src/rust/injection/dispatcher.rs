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

use super::fallback::{locate_on_path, select_helper, LinuxSession};
use super::paste::PasteShortcut;
#[cfg(not(target_os = "linux"))]
use super::wayland::target_prefers_terminal_paste;
#[cfg(target_os = "linux")]
use super::wayland::{paste_shortcut, target_prefers_terminal_paste, type_text as wayland_type};

/// Which strategy to use for a single injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectMethod {
    /// Direct key-event injection. Slow but reliable for plain text.
    Typing,
    /// Copy to clipboard, send paste keystroke, restore previous clipboard.
    Paste(PasteShortcut),
}

impl Default for InjectMethod {
    fn default() -> Self {
        InjectMethod::Paste(PasteShortcut::default())
    }
}

/// High-level entry point. Construction is cheap — no system calls, no
/// helper-binary lookups. The actual injection happens in [`Injector::inject_text`].
#[derive(Debug, Clone)]
pub struct Injector {
    target_title: String,
    target_process: String,
    xkb_layout: String,
}

impl Injector {
    pub fn new() -> Self {
        Injector {
            target_title: String::new(),
            target_process: String::new(),
            xkb_layout: String::new(),
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

    /// Run an injection. Dispatch:
    ///
    /// * **Windows / macOS** — always go through enigo (requires the
    ///   `rust-injection` feature, since `enigo` is an optional dep).
    /// * **Linux** — choose the first available helper in the per-session
    ///   chain ([`fallback::select_helper`]). Today this delegates to the
    ///   existing `wayland.rs` path (`ydotool`) when `ydotool` is the pick;
    ///   the other helpers (`kwtype`, `wtype`, `dotool`, `xdotool`) get a
    ///   best-effort `Command::new(helper).args(...)` invocation.
    pub fn inject_text(&self, text: &str, method: InjectMethod) -> Result<()> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            let _ = self; // keep `self` used on platforms without the linux branch
            inject_via_enigo(text, method)
        }
        #[cfg(target_os = "linux")]
        {
            self.inject_on_linux(text, method)
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = (self, text, method);
            Err(anyhow!("unsupported platform for rust injection"))
        }
    }

    #[cfg(target_os = "linux")]
    fn inject_on_linux(&self, text: &str, method: InjectMethod) -> Result<()> {
        // ydotool already has a fully-featured layout-aware code path in
        // wayland.rs — reuse it when ydotool wins the chain. The other helpers
        // get a generic invocation through super::linux_helpers.
        use super::linux_helpers::{invoke_paste, invoke_type};

        let session = LinuxSession::detect();
        let helper = select_helper(session, locate_on_path).ok_or_else(|| {
            anyhow!(
                "no Linux injection helper found on PATH (tried: {:?})",
                super::fallback::fallback_chain(session)
            )
        })?;

        match method {
            InjectMethod::Typing => {
                if helper == "ydotool" {
                    wayland_type(text, &self.xkb_layout)
                } else {
                    invoke_type(helper, text)
                }
            }
            InjectMethod::Paste(shortcut) => {
                if helper == "ydotool" {
                    paste_shortcut(&self.target_title, &self.target_process)
                } else {
                    let chosen = if shortcut == PasteShortcut::default() {
                        PasteShortcut::for_linux_target(target_prefers_terminal_paste(
                            &self.target_title,
                            &self.target_process,
                        ))
                    } else {
                        shortcut
                    };
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

#[cfg(any(windows, target_os = "macos"))]
fn inject_via_enigo(text: &str, method: InjectMethod) -> Result<()> {
    let mut backend = super::enigo_backend::make_default_backend()?;
    match method {
        InjectMethod::Typing => backend.type_text(text),
        InjectMethod::Paste(shortcut) => {
            // The dispatcher doesn't own the clipboard here; the Python
            // worker still drives pyperclip and merely asks us to send the
            // keystroke. Rust-side clipboard ownership is wired by the
            // PasteGuard in paste.rs and is exercised by unit tests; this
            // arm avoids double-copy when Python already populated the
            // clipboard via the existing _paste() path.
            super::enigo_backend::send_paste_shortcut(backend.as_mut(), shortcut)
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
            let injector = Injector::new()
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
            let shortcut = match spec.shortcut.as_deref() {
                None | Some("") => PasteShortcut::default(),
                Some(raw) => PasteShortcut::parse(raw)
                    .ok_or_else(|| anyhow!("unknown paste shortcut: {raw}"))?,
            };
            InjectMethod::Paste(shortcut)
        }
    })
}

fn method_label(method: InjectMethod) -> String {
    match method {
        InjectMethod::Typing => "typing".to_owned(),
        InjectMethod::Paste(s) => format!("paste:{}", paste_label(s)),
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
    fn resolve_method_defaults_to_paste_with_platform_shortcut() {
        let spec = InjectMethodSpec::default();
        let method = resolve_method(&spec).unwrap();
        match method {
            InjectMethod::Paste(s) => assert_eq!(s, PasteShortcut::default()),
            _ => panic!("expected paste"),
        }
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
            InjectMethod::Paste(PasteShortcut::CtrlShiftV)
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
            method_label(InjectMethod::Paste(PasteShortcut::CtrlShiftV)),
            "paste:ctrl_shift_v"
        );
        assert_eq!(method_label(InjectMethod::Typing), "typing");
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
}
