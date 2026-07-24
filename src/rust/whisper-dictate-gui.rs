//! GUI-only entry point (`whisper-dictate-gui.exe`). Windows-subsystem on
//! Windows so a double-click from Explorer / a tray shortcut / autostart
//! never flashes a cmd window. Every CLI verb lives in the sibling
//! `whisper-dictate.exe` binary (console subsystem); this one has a single
//! purpose: launch the tray/settings UI. Both binaries delegate to the same
//! shared library crate (`whisper_dictate_app`) so the backend logic is
//! written once and reused.
//!
//! The exit-code + stderr-on-error handling is factored out into
//! `whisper_dictate_app::entrypoint::error_exit_shell` (unit-tested there),
//! so `main` here is a single call — the smallest possible untestable
//! Rust entrypoint.
//!
//! Since this binary has no console attached in release, the shell's
//! stderr write is a best-effort — the caller (a shortcut/autostart)
//! usually has nowhere to display it. The UI itself surfaces user-facing
//! failures through its own dialogs, so the error branch here only fires
//! on early startup crashes before the UI has a window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    whisper_dictate_app::entrypoint::error_exit_shell(
        "error",
        std::io::stderr(),
        whisper_dictate_app::ui::run,
    )
}
