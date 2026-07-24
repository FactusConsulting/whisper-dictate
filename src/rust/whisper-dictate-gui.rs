//! GUI-only entry point (`whisper-dictate-gui.exe`). Windows-subsystem on
//! Windows so a double-click from Explorer / a tray shortcut / autostart
//! never flashes a cmd window. Every CLI verb lives in the sibling
//! `whisper-dictate.exe` binary (console subsystem); this one has a single
//! purpose: launch the tray/settings UI. Both binaries delegate to the same
//! shared library crate (`whisper_dictate_app`) so the backend logic is
//! written once and reused.
//!
//! Since this binary has no console attached in release, `eprintln!` on the
//! error path is a best-effort — the caller (a shortcut/autostart) usually
//! has nowhere to display it. The UI itself surfaces user-facing failures
//! through its own dialogs, so the error branch here only fires on early
//! startup crashes before the UI has a window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    if let Err(err) = whisper_dictate_app::ui::run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
