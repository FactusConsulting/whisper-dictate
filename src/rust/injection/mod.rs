//! Text-injection backend.
//!
//! Phase 1 (shipped): the `inject-text` hidden subcommand wraps the existing
//! Wayland `ydotool` keymap path so the Python worker can shell out for
//! layout-correct typing on Linux. Lives in [`wayland`] + [`keymap`].
//!
//! Phase 2.1 (this module): a cross-platform `enigo`-backed [`Injector`]
//! (Windows + macOS + Linux/X11) with a Linux Wayland helper fallback chain
//! (KDE → `kwtype`, other Wayland → `wtype`, then `dotool`, then `ydotool`;
//! X11 → `xdotool` → `ydotool`). Layout-independent paste shortcuts via
//! platform VK codes and clipboard save/restore live in [`paste`]. Backend
//! detection lives in [`fallback`]. The whole Phase 2.1 path is opt-in: the
//! enigo dep is gated behind the `rust-injection` cargo feature AND the
//! `VOICEPI_INJECTION_BACKEND=rust` env var.
//!
//! The public CLI surface is `whisper-dictate inject` ([`dispatcher::handle_inject`])
//! which reads a JSON request envelope on stdin and writes a JSON response
//! on stdout, mirroring the `health` subcommand pattern.

pub mod dispatcher;
pub mod enigo_backend;
pub mod fallback;
pub mod keymap;
#[cfg(target_os = "linux")]
pub mod linux_helpers;
pub mod paste;
pub mod plan;
pub mod self_test;
pub mod wayland;

pub use dispatcher::{
    handle_inject, InjectMethod, InjectMethodSpec, InjectMode, InjectRequest, InjectResponse,
    Injector, ProbeResponse,
};
pub use fallback::{fallback_chain, select_helper, LinuxSession};
pub use paste::{Clipboard, PasteGuard, PasteShortcut};
pub use plan::{
    build_plan, execute_plan, pick_backend, plan_keystrokes, resolve_mode, InjectionPlan, PlanMode,
};
pub use wayland::{
    build_ydotool_ops, paste_shortcut_args, target_prefers_terminal_paste, YdotoolOp,
};

use anyhow::{anyhow, Result};

/// Phase 1 entry point: keeps the existing hidden `inject-text` subcommand
/// working. Delegates straight to the `wayland` ydotool path.
pub fn handle_inject_text(
    mode: &str,
    text: &str,
    xkb_layout: &str,
    target_title: &str,
    target_process: &str,
) -> Result<()> {
    match mode {
        "type" => wayland::type_text(text, xkb_layout),
        "paste" => wayland::paste_shortcut(target_title, target_process),
        other => Err(anyhow!("unsupported inject-text mode: {other}")),
    }
}

/// Public `inject-text <TEXT>` verb — audit item 2 chunk B. Scripting /
/// smoke-test wrapper over the injection library.
///
/// The default is a **dry-run** that prints the resolved plan (backend,
/// mode, character stream) and does NOT touch the display. Real injection
/// requires `--do-it` (or `--live`).
///
/// * `dry_run=true` → print the plan and return `Ok(())`.
/// * `dry_run=false` (i.e. `--do-it` was passed) → run the plan through
///   [`plan::execute_plan`], updating `typed=true` on success before
///   printing the response. Errors surface as `Err` so the CLI exits
///   non-zero.
///
/// `json=true` prints the [`InjectionPlan`] as a single JSON line;
/// `json=false` prints a human-readable summary. Both shapes are stable —
/// the smoke script pins the JSON keys and the plain-text prefixes.
pub fn handle_public_inject_text(
    text: &str,
    backend: &str,
    dry_run: bool,
    do_it: bool,
    json: bool,
    target_title: &str,
    target_process: &str,
) -> Result<()> {
    // Conflict guard: `--dry-run --do-it` is ambiguous. Reject it explicitly
    // so a user who typed both by mistake never accidentally injects.
    if dry_run && do_it {
        return Err(anyhow!(
            "conflicting flags: --dry-run and --do-it cannot both be set"
        ));
    }
    let will_execute = do_it;
    let os = std::env::consts::OS;
    let session = LinuxSession::detect();
    let mut planned = plan::build_plan(text, backend, os, session, !will_execute)?;

    if will_execute {
        // Print a loud warning BEFORE injecting so the operator sees which
        // window will be typed into if this was a mistake. Warning goes to
        // stderr so `--json` output on stdout stays a single parseable line.
        eprintln!(
            "warning: `inject-text --do-it` is REAL and will type into the active window \
             using backend `{}` (mode: {}). Focus the target window NOW.",
            planned.backend, planned.mode,
        );
        plan::execute_plan(&planned, target_title, target_process)?;
        planned.typed = true;
    }

    print_plan(&planned, json)
}

fn print_plan(plan: &plan::InjectionPlan, json: bool) -> Result<()> {
    if json {
        // One line, machine-readable — the smoke script pipes this straight
        // into a `grep` for the JSON keys.
        println!("{}", serde_json::to_string(plan)?);
    } else {
        // Human-readable — stable prefixes so downstream shell tests can
        // grep them. Keep it terse; the JSON shape is the canonical form.
        let status = if plan.typed {
            "INJECTED"
        } else {
            "DRY-RUN (nothing typed)"
        };
        println!("[inject-text] {status}");
        println!("  text:     {:?}", plan.text);
        println!("  backend:  {}", plan.backend);
        println!("  mode:     {}", plan.mode);
        println!("  chars:    {}", plan.chars);
        println!(
            "  keys:     [{}]",
            plan.planned_keystrokes
                .iter()
                .map(|k| format!("{k:?}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    Ok(())
}
