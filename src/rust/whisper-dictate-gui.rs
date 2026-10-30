//! GUI entry point for `wd-gui`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    // Windows-only PTT hotkey driver default: prefer `RegisterHotKey` over
    // the WH_KEYBOARD_LL hook chain. In a GUI-subsystem process third-party
    // LL hooks (Steam / Logitech Options+ / G HUB screen-capture tools)
    // filter function keys, Ctrl, and Pause out of the chain before rdev's
    // hook sees them; `WM_HOTKEY` is routed by USER32 after the LL chain
    // runs, so upstream consume-decisions cannot block it. See
    // `src/rust/hotkey/manager/win_registerhotkey.rs` for the driver and
    // its limitations (modifier-only chords fall back to rdev).
    //
    // Only set the default if the user hasn't pinned a driver, so
    // `VOICEPI_HOTKEY_DRIVER=rdev` still wins for a bare-modifier binding.
    //
    // This MUST stay the first statement of `main`: `set_var` is only sound
    // while the process is single-threaded, and both the diagnostic writer
    // and the WH_KEYBOARD_LL pump below spawn threads. The log line is
    // emitted later, once the diagnostic sink exists.
    #[cfg(windows)]
    let defaulted_hotkey_driver = if std::env::var_os("VOICEPI_HOTKEY_DRIVER").is_none() {
        std::env::set_var("VOICEPI_HOTKEY_DRIVER", "register");
        true
    } else {
        false
    };

    // The guided verifier respawns THIS executable so the bounded listener
    // keeps the release GUI subsystem on Windows. Dispatch that hidden child
    // mode before starting diagnostics or the eframe event loop.
    if whisper_dictate_app::runtime::hotkey_probe_child_requested() {
        return whisper_dictate_app::entrypoint::error_exit_shell_with_teardown(
            "error",
            std::io::stderr(),
            whisper_dictate_app::runtime::run_hotkey_probe_child,
        );
    }

    // Install the Windows-only diagnostic log BEFORE the UI starts so
    // any `crate::diag::log!` line emitted during startup (config
    // parse, hotkey install, Phase-B fallback, ...) lands in the file.
    // Failures are silently swallowed — a missing diagnostic must not
    // stop the GUI from starting. See the module docs on
    // `whisper_dictate_app::diag` for the contract.
    #[cfg(windows)]
    if let Some(path) = whisper_dictate_app::diag::default_gui_diagnostic_path() {
        if whisper_dictate_app::diag::install_gui_diagnostic_log(&path).is_ok() {
            // Capture panics as soon as the tee file is usable. The remaining
            // startup work can parse environment, write markers, and start
            // helper threads, all of which are fallible.
            whisper_dictate_app::diag::install_gui_panic_hook();
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

    #[cfg(windows)]
    if defaulted_hotkey_driver {
        whisper_dictate_app::diag::log!(
            "[gui] defaulted VOICEPI_HOTKEY_DRIVER=register to bypass the \
             WH_KEYBOARD_LL hook chain (set the env var to rdev/evdev \
             explicitly to override)"
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
