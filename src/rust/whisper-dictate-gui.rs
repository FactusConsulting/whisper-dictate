//! GUI-only entry point (`whisper-dictate-gui.exe`). Windows-subsystem on
//! Windows so a double-click from Explorer / a tray shortcut / autostart
//! never flashes a cmd window. Every CLI verb lives in the sibling
//! `whisper-dictate.exe` binary (console subsystem); this one has a single
//! purpose: launch the tray/settings UI. Both binaries delegate to the same
//! shared library crate (`whisper_dictate_app`) so the backend logic is
//! written once and reused.
//!
//! The exit-code + stderr-on-error handling is factored out into
//! `whisper_dictate_app::entrypoint::error_exit_shell_with_teardown`
//! (unit-tested there),
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
            // Resolve the diagnostic level BEFORE emitting the startup
            // marker so the marker line itself records what level the
            // rest of the session will be logged at. Support-thread
            // triage: "did the operator run with `basic` or `deep`?" is
            // the very first question on a Windows PTT wedge report.
            let level = whisper_dictate_app::diag::init_from_env();
            // One session-marker line so the append boundary is visible
            // when this launch's lines are mixed with the previous run's
            // in the same file. Includes the binary version so a support
            // thread can immediately see which rc/release the operator
            // is on, and the diag level so the reader knows which layers
            // of trace to expect further down the file.
            whisper_dictate_app::diag::log!(
                "[gui] whisper-dictate-gui {} starting; {}={}; diagnostic log at {}",
                env!("CARGO_PKG_VERSION"),
                whisper_dictate_app::diag::LOG_ENV_VAR,
                level.as_str(),
                path.display(),
            );
            // Install the parallel WH_KEYBOARD_LL diagnostic hook if
            // (and only if) the operator opted in via
            // VOICEPI_LOG=trace. The function is a no-op below
            // that gate so unconditional invocation is safe. It spawns
            // its own dedicated pump thread and runs for the process
            // lifetime — LL hooks cannot be safely uninstalled without
            // ending the message-pump thread. See the module docs on
            // `hotkey::manager::win_raw_hook` for the F9-drop
            // investigation this feeds.
            if whisper_dictate_app::hotkey::manager::win_raw_hook::install() {
                whisper_dictate_app::diag::log!(
                    "[gui] parallel WH_KEYBOARD_LL diagnostic hook installed \
                     alongside rdev's own hook - see [win/raw-hook] lines below"
                );
            }
        }
    }

    // Windows-only PTT hotkey driver default: bypass the WH_KEYBOARD_LL
    // hook chain by preferring `RegisterHotKey`. Diagnosed on rc.10
    // (PR #646 GUI diagnostic log): with the default rdev backend, the
    // GUI-subsystem process context lost function keys, Ctrl, and Pause
    // to third-party LL hooks (Steam / Logitech Options+ / G HUB /
    // screen-capture tools) that filter those events out of the chain
    // before our hook sees them — letters, digits, Shift, and the
    // Windows key still reached rdev, but the chord keys never did.
    // The same binary running the CLI `dictate-run` verb (console
    // subsystem, no GUI-scope LL hooks attached) captures every key
    // fine, so the fault is specific to GUI-subsystem process context,
    // not the rdev crate itself.
    //
    // `RegisterHotKey` delivers `WM_HOTKEY` through USER32's message
    // routing AFTER the LL-hook chain runs, so consume-decisions
    // upstream don't block it. See
    // `src/rust/hotkey/manager/win_registerhotkey.rs` for the driver
    // and the limitations table (modifier-only chords are not
    // supported; the install path falls back to rdev in that case).
    //
    // Only set the default if the user hasn't already pinned a
    // driver — a `VOICEPI_HOTKEY_DRIVER=rdev` escape hatch must
    // still win so a user with a bare-modifier binding can force
    // the old backend explicitly.
    #[cfg(windows)]
    if std::env::var_os("VOICEPI_HOTKEY_DRIVER").is_none() {
        std::env::set_var("VOICEPI_HOTKEY_DRIVER", "register");
        whisper_dictate_app::diag::log!(
            "[gui] defaulted VOICEPI_HOTKEY_DRIVER=register to bypass the \
             WH_KEYBOARD_LL hook chain (rc.10 diagnostic fix; set the env \
             var to rdev/evdev explicitly to override)"
        );
    }

    // `_with_teardown`, never the bare shell: it drains the async
    // diagnostic queue before the process exits, so a PTT wedge repro the
    // operator just captured is durable in the tee file rather than dying
    // with the background writer thread.
    whisper_dictate_app::entrypoint::error_exit_shell_with_teardown(
        "error",
        std::io::stderr(),
        whisper_dictate_app::ui::run,
    )
}
