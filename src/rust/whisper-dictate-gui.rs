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
//!
//! ## Windows diagnostic log
//!
//! Because `windows_subsystem = "windows"` detaches from any parent
//! console, every `eprintln!` from the rdev listener, supervisor
//! Phase-B branches, and `[hotkey] ...` diagnostics goes to a discarded
//! stderr handle. That is the "Stderr is silent (0 bytes) even with
//! RUST_LOG=debug + VOICEPI_HOTKEY_DEBUG=1" symptom Windows PTT bug
//! reports carry. This binary opens a tee log at
//! `%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log` at startup via
//! [`whisper_dictate_app::diag::install_gui_diagnostic_log`] so future
//! Windows PTT wedges are inspectable after the fact without a rebuild
//! — every `crate::diag::log!` call (the hotkey install path, the
//! supervisor's Phase-B branches) tees there in addition to the
//! discarded stderr. The CLI binary does NOT install this because it
//! keeps a console-attached stderr and does not need the tee.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    // Install the Windows-only diagnostic log BEFORE the UI starts so
    // any `crate::diag::log!` line emitted during startup (config
    // parse, hotkey install, Phase-B fallback, ...) lands in the file.
    // Failures are silently swallowed — a missing diagnostic must not
    // stop the GUI from starting. See the module docs on
    // `whisper_dictate_app::diag` for the contract.
    #[cfg(windows)]
    if let Some(path) = whisper_dictate_app::diag::default_gui_diagnostic_path() {
        if whisper_dictate_app::diag::install_gui_diagnostic_log(&path).is_ok() {
            // One session-marker line so the append boundary is visible
            // when this launch's lines are mixed with the previous run's
            // in the same file. Includes the binary version so a support
            // thread can immediately see which rc/release the operator
            // is on.
            whisper_dictate_app::diag::log!(
                "[gui] whisper-dictate-gui {} starting; diagnostic log at {}",
                env!("CARGO_PKG_VERSION"),
                path.display(),
            );
        }
    }

    whisper_dictate_app::entrypoint::error_exit_shell(
        "error",
        std::io::stderr(),
        whisper_dictate_app::ui::run,
    )
}
