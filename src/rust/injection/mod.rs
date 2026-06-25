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
pub mod wayland;

pub use dispatcher::{
    handle_inject, InjectMethod, InjectMethodSpec, InjectMode, InjectRequest, InjectResponse,
    Injector, ProbeResponse,
};
pub use fallback::{fallback_chain, select_helper, LinuxSession};
pub use paste::{Clipboard, PasteGuard, PasteShortcut};
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
