//! Public `inject-text <TEXT>` planning + execution surface.
//!
//! Wraps the existing injection library ([`Injector`], `wayland.rs`, the Linux
//! helper chain) with a scripting/smoke-testable CLI verb. The default is a
//! **dry-run** that computes the keystroke plan (which backend would be
//! picked, character stream, paste chord) and prints it as JSON or plain
//! text — nothing is actually typed. Real injection is opt-in via `--do-it`.
//!
//! The planning half is pure logic (no I/O, no dep on `enigo` / `ydotool`),
//! so the unit tests here can exercise the full backend-selection matrix and
//! the character-stream expansion on every platform without a display server.
//!
//! Audit item 2 chunk B — public CLI wrapper for the injection feature
//! (`docs/architecture-audit-2026-07-16.md`).

use anyhow::{anyhow, Result};
use serde::Serialize;

use super::dispatcher::{InjectMethod, Injector};
use super::fallback::{fallback_chain, LinuxSession};
use super::paste::PasteShortcut;

/// The requested injection mode. Independent of backend — a `Paste` request
/// still needs a backend (enigo / helper chain / pynput) to fire the chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMode {
    /// Send each character as a synthetic key event.
    Typing,
    /// Copy to the clipboard and press the paste chord (`Ctrl+V` /
    /// `Cmd+V` / terminal-aware `Ctrl+Shift+V` on Linux).
    Paste,
}

impl PlanMode {
    /// Serialised name used in the JSON output. Kept stable — the smoke
    /// script and downstream tooling grep for these literals.
    pub fn as_str(self) -> &'static str {
        match self {
            PlanMode::Typing => "type",
            PlanMode::Paste => "paste",
        }
    }
}

/// The dry-run report — round-trippable via serde so the JSON output has a
/// stable shape (see the audit doc's `--json` example).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InjectionPlan {
    pub text: String,
    /// Resolved backend name — `pynput`, `wtype`, `ydotool`, `enigo`, ...
    /// When the user passed `--backend auto` this is the first candidate the
    /// matrix picked; when they pinned a specific backend it's echoed back so
    /// operators see exactly what would run.
    pub backend: String,
    /// `type` or `paste`.
    pub mode: String,
    /// Character count of `text` (Unicode scalar values, not bytes).
    pub chars: usize,
    /// The stream of keystroke tokens the backend would emit, in order.
    /// For typing mode this is one entry per character (Unicode grapheme
    /// approximation — sufficient for smoke-testing intent). For paste mode
    /// this is `["clipboard:<text>", "<paste-chord>"]` — the exact wording
    /// is stable so tests can pin it.
    pub planned_keystrokes: Vec<String>,
    pub dry_run: bool,
    pub typed: bool,
}

/// Backend selection matrix. Given the user's `--backend` choice, the host
/// OS, and the Linux session (if applicable), returns the resolved backend
/// name that would run. Pure function — no `$PATH` lookups, no env reads
/// (the caller feeds those in) — so the test matrix can exercise every arm.
///
/// Accepted values for `requested`:
///
/// * `auto` — pick per platform. Windows → `pynput` (the shipping default
///   in `vp_inject.py`), macOS → `pynput`, Linux Wayland/X11 → first entry
///   of the [`fallback_chain`] for the session (`wtype` on generic Wayland,
///   `xdotool` on X11, `kwtype` on KDE Wayland).
/// * `pynput`, `wtype`, `ydotool`, `xdotool`, `kwtype`, `dotool`, `enigo` —
///   pinned; returned verbatim.
/// * `type`, `paste` — MODE selectors (not backends). Treated as `auto` for
///   backend resolution; the mode is applied separately by [`resolve_mode`].
pub fn pick_backend(requested: &str, os: &str, linux_session: LinuxSession) -> Result<String> {
    let normalised = requested.trim().to_lowercase();
    match normalised.as_str() {
        // Mode-aliases fall through to auto backend picking.
        "" | "auto" | "type" | "paste" => Ok(auto_backend_for(os, linux_session)),
        // Explicit backends: echo back verbatim (validated against the set
        // clap already accepts, so unknown strings never reach here).
        "pynput" | "wtype" | "ydotool" | "xdotool" | "kwtype" | "dotool" | "enigo" => {
            Ok(normalised)
        }
        other => Err(anyhow!("unknown backend: {other}")),
    }
}

/// The `auto` picker — factored out so [`pick_backend`] and the tests can
/// share one source of truth.
fn auto_backend_for(os: &str, linux_session: LinuxSession) -> String {
    match os {
        "linux" => fallback_chain(linux_session)
            .first()
            .copied()
            .unwrap_or("wtype")
            .to_owned(),
        // Windows and macOS ship on the Python `pynput` path today (see
        // `vp_inject.py`); the Rust `enigo` backend is opt-in via
        // `VOICEPI_INJECTION_BACKEND=rust`. Reflect the shipping default
        // here so `--dry-run` reports what would ACTUALLY run.
        "windows" | "macos" => "pynput".to_owned(),
        // Unknown platform — best effort. `enigo` is the most portable
        // Rust-side option, so surface it explicitly rather than pretending
        // Wayland helpers apply.
        _ => "enigo".to_owned(),
    }
}

/// Resolve the mode from the user's `--backend` choice: `type` / `paste` are
/// MODE aliases (they don't name a backend). Everything else defaults to the
/// safer `type` mode — pastes touch the user's clipboard and are the more
/// invasive choice.
pub fn resolve_mode(requested: &str) -> PlanMode {
    match requested.trim().to_lowercase().as_str() {
        "paste" => PlanMode::Paste,
        _ => PlanMode::Typing,
    }
}

/// Expand `text` into the token stream the caller would send. Pure — no
/// backend involvement — so the unit tests can pin every character
/// (including Danish `æøå` and emoji) without a display server.
pub fn plan_keystrokes(text: &str, mode: PlanMode) -> Vec<String> {
    match mode {
        PlanMode::Typing => text.chars().map(|c| c.to_string()).collect(),
        PlanMode::Paste => {
            // Two-step plan: (1) populate the clipboard with the full text,
            // (2) fire the paste chord. Wording is stable so smoke tests
            // can grep it verbatim.
            let chord = platform_paste_chord();
            vec![format!("clipboard:{text}"), chord.to_owned()]
        }
    }
}

/// The paste chord that would fire in `--dry-run`. Uses the platform-default
/// [`PasteShortcut`] — the same one [`InjectMethod::Paste(None)`] would
/// pick at runtime. Format matches the human-readable label users see in
/// documentation (`Ctrl+V`, `Cmd+V`, `Ctrl+Shift+V`).
pub fn platform_paste_chord() -> &'static str {
    match PasteShortcut::default() {
        PasteShortcut::CmdV => "Cmd+V",
        PasteShortcut::CtrlV => "Ctrl+V",
        PasteShortcut::CtrlShiftV => "Ctrl+Shift+V",
        PasteShortcut::ShiftInsert => "Shift+Insert",
    }
}

/// Build the full [`InjectionPlan`] for a dry-run. Wraps [`pick_backend`]
/// and [`plan_keystrokes`] — the CLI handler calls this once and either
/// prints the plan (dry-run) or hands off to [`execute_plan`] (real inject).
pub fn build_plan(
    text: &str,
    requested_backend: &str,
    os: &str,
    linux_session: LinuxSession,
    dry_run: bool,
) -> Result<InjectionPlan> {
    let backend = pick_backend(requested_backend, os, linux_session)?;
    let mode = resolve_mode(requested_backend);
    let keystrokes = plan_keystrokes(text, mode);
    Ok(InjectionPlan {
        text: text.to_owned(),
        backend,
        mode: mode.as_str().to_owned(),
        chars: text.chars().count(),
        planned_keystrokes: keystrokes,
        dry_run,
        typed: false,
    })
}

/// Real-inject entry point. Delegates to the existing [`Injector`] path
/// (same code the JSON envelope dispatcher runs) so this verb never grows
/// its own injection code path — it only wraps.
///
/// Note: `pynput` is a Python backend and has no Rust in-process
/// implementation; asking for `--do-it --backend pynput` returns a clear
/// error rather than silently switching backends.
pub fn execute_plan(plan: &InjectionPlan, target_title: &str, target_process: &str) -> Result<()> {
    if plan.backend == "pynput" {
        return Err(anyhow!(
            "backend `pynput` is Python-only; use `vp_inject.py` or run the worker directly \
             (dry-run reports the plan but --do-it via this CLI is Rust-side only)"
        ));
    }
    let method = match plan.mode.as_str() {
        "type" => InjectMethod::Typing,
        "paste" => InjectMethod::Paste(None),
        other => return Err(anyhow!("unknown mode in plan: {other}")),
    };
    let mut injector = Injector::new().with_target(target_title, target_process);
    injector.inject_text(&plan.text, method)
}

// ---------------------------------------------------------------------------
// Tests — pure planning + backend matrix. No I/O, no display server.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_backend_windows_is_pynput() {
        // Windows ships on the Python pynput path today; `--dry-run` must
        // reflect that so the user isn't surprised by the reported backend.
        assert_eq!(
            pick_backend("auto", "windows", LinuxSession::Unknown).unwrap(),
            "pynput"
        );
    }

    #[test]
    fn auto_backend_macos_is_pynput() {
        assert_eq!(
            pick_backend("auto", "macos", LinuxSession::Unknown).unwrap(),
            "pynput"
        );
    }

    #[test]
    fn auto_backend_wayland_prefers_wtype_over_ydotool() {
        // Chain for OtherWayland is [wtype, dotool, ydotool] — first entry
        // wins so an installed wtype gets picked before the older ydotool.
        assert_eq!(
            pick_backend("auto", "linux", LinuxSession::OtherWayland).unwrap(),
            "wtype"
        );
    }

    #[test]
    fn auto_backend_kde_wayland_prefers_kwtype() {
        assert_eq!(
            pick_backend("auto", "linux", LinuxSession::KdeWayland).unwrap(),
            "kwtype"
        );
    }

    #[test]
    fn auto_backend_x11_prefers_xdotool() {
        assert_eq!(
            pick_backend("auto", "linux", LinuxSession::X11).unwrap(),
            "xdotool"
        );
    }

    #[test]
    fn explicit_backend_is_echoed_verbatim() {
        for name in [
            "pynput", "wtype", "ydotool", "xdotool", "kwtype", "dotool", "enigo",
        ] {
            assert_eq!(
                pick_backend(name, "linux", LinuxSession::OtherWayland).unwrap(),
                name
            );
        }
    }

    #[test]
    fn mode_aliases_fall_through_to_auto_backend() {
        // `type` and `paste` name a MODE not a backend — they must not be
        // echoed as a backend name (`build_plan` returns the resolved
        // fallback-chain pick instead).
        assert_eq!(
            pick_backend("type", "linux", LinuxSession::OtherWayland).unwrap(),
            "wtype"
        );
        assert_eq!(
            pick_backend("paste", "linux", LinuxSession::OtherWayland).unwrap(),
            "wtype"
        );
    }

    #[test]
    fn unknown_backend_is_rejected() {
        assert!(pick_backend("nope", "linux", LinuxSession::OtherWayland).is_err());
    }

    #[test]
    fn resolve_mode_defaults_to_typing() {
        assert_eq!(resolve_mode("auto"), PlanMode::Typing);
        assert_eq!(resolve_mode(""), PlanMode::Typing);
        assert_eq!(resolve_mode("wtype"), PlanMode::Typing);
        assert_eq!(resolve_mode("type"), PlanMode::Typing);
    }

    #[test]
    fn resolve_mode_picks_paste_only_for_paste_alias() {
        assert_eq!(resolve_mode("paste"), PlanMode::Paste);
        assert_eq!(resolve_mode("PASTE"), PlanMode::Paste); // case-insensitive
    }

    #[test]
    fn plan_keystrokes_typing_yields_one_entry_per_char() {
        assert_eq!(
            plan_keystrokes("hej", PlanMode::Typing),
            vec!["h", "e", "j"]
        );
    }

    #[test]
    fn plan_keystrokes_typing_preserves_unicode_scalars() {
        // Danish `æøå` and a plain emoji must round-trip as single tokens —
        // this is what a downstream test would assert to confirm the CLI
        // did not silently drop non-ASCII.
        let plan = plan_keystrokes("æøå🚀", PlanMode::Typing);
        assert_eq!(plan, vec!["æ", "ø", "å", "🚀"]);
    }

    #[test]
    fn plan_keystrokes_paste_reports_clipboard_and_chord() {
        let plan = plan_keystrokes("hej", PlanMode::Paste);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], "clipboard:hej");
        // Chord is platform-dependent — just assert it's one of the known
        // labels so the test runs on every host without a cfg dance.
        assert!(
            matches!(
                plan[1].as_str(),
                "Ctrl+V" | "Cmd+V" | "Ctrl+Shift+V" | "Shift+Insert"
            ),
            "unexpected paste chord: {}",
            plan[1]
        );
    }

    #[test]
    fn build_plan_dry_run_never_marks_typed_true() {
        // Whether dry_run is on or off, `typed` starts false — it only
        // flips to true after `execute_plan` succeeds (the CLI handler
        // updates it before printing). This guards the smoke script's
        // assertion that `dry_run=true` implies `typed=false`.
        let plan = build_plan("hi", "auto", "linux", LinuxSession::OtherWayland, true).unwrap();
        assert!(plan.dry_run);
        assert!(!plan.typed);
        assert_eq!(plan.chars, 2);
        assert_eq!(plan.backend, "wtype");
        assert_eq!(plan.mode, "type");
    }

    #[test]
    fn build_plan_paste_alias_produces_paste_mode_plan() {
        let plan = build_plan("hi", "paste", "linux", LinuxSession::OtherWayland, true).unwrap();
        assert_eq!(plan.mode, "paste");
        // Backend still resolves via the fallback chain (the mode alias
        // does not name a backend).
        assert_eq!(plan.backend, "wtype");
        assert_eq!(plan.planned_keystrokes.len(), 2);
        assert!(plan.planned_keystrokes[0].starts_with("clipboard:"));
    }

    #[test]
    fn build_plan_pinned_backend_is_preserved() {
        // Regression guard: pinning `--backend ydotool` must survive
        // build_plan (i.e. it doesn't get overwritten by an auto pick).
        let plan = build_plan("hi", "ydotool", "linux", LinuxSession::OtherWayland, true).unwrap();
        assert_eq!(plan.backend, "ydotool");
    }

    #[test]
    fn build_plan_empty_text_still_produces_a_valid_plan() {
        // Empty payload is a legal dry-run — the plan reports 0 chars and
        // an empty keystroke stream so a downstream test can assert the
        // CLI handled the no-op cleanly.
        let plan = build_plan("", "auto", "linux", LinuxSession::OtherWayland, true).unwrap();
        assert_eq!(plan.chars, 0);
        assert!(plan.planned_keystrokes.is_empty());
    }

    #[test]
    fn execute_plan_rejects_pynput_backend() {
        // Real-inject via pynput would require shelling out to Python; the
        // Rust CLI verb never does that today. The dry-run still reports
        // `backend=pynput` (that's what the shipping worker would run),
        // but `--do-it --backend pynput` must return a clear error rather
        // than silently switching to `enigo`.
        let plan = build_plan("hi", "pynput", "windows", LinuxSession::Unknown, false).unwrap();
        let err = execute_plan(&plan, "", "").unwrap_err();
        assert!(
            err.to_string().contains("pynput"),
            "error should mention pynput: {err}"
        );
    }

    #[test]
    fn plan_serialises_to_stable_json_shape() {
        // Downstream tests grep for these top-level keys — pin the shape.
        let plan = build_plan("hi", "auto", "linux", LinuxSession::OtherWayland, true).unwrap();
        let json = serde_json::to_string(&plan).unwrap();
        for key in [
            "\"text\":",
            "\"backend\":",
            "\"mode\":",
            "\"chars\":",
            "\"planned_keystrokes\":",
            "\"dry_run\":",
            "\"typed\":",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
    }
}
