//! Stable request/response envelopes and method selection for the JSON CLI.

use super::InjectMethod;
use crate::injection::paste::PasteShortcut;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Serialize)]
pub struct ProbeResponse {
    pub platform: String,
    pub linux_session: Option<String>,
    pub linux_helper: Option<String>,
    pub feature_enabled: bool,
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

pub(super) fn response_for_outcome(
    method: InjectMethod,
    outcome: super::InjectOutcome,
) -> InjectResponse {
    match outcome.result {
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
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
