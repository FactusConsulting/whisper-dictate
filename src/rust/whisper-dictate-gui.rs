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
            // Codex P2 #651 discussion PRRT_kwDOSfNjQs6UT1qZ: the
            // `VOICEPI_LOG=trace` boundary-trace decision tree assumes
            // the rdev listener is running so it can consult
            // `[rdev/callback]` / `[chord]` lines. But the GUI defaults
            // `VOICEPI_HOTKEY_DRIVER=register` a few lines below, which
            // bypasses the rdev listener entirely — so the trace docs
            // silently produce a false diagnosis for anyone running the
            // trace without pinning the driver. Emit a warning when the
            // two env vars combine that way so the operator knows to
            // set `VOICEPI_HOTKEY_DRIVER=rdev` explicitly. Deliberately
            // does NOT override the driver (the user may have a good
            // reason to keep RegisterHotKey active while investigating
            // an unrelated symptom); a loud, actionable warning beats
            // a silent behaviour change.
            let voicepi_log = std::env::var("VOICEPI_LOG").ok();
            let driver_val = std::env::var("VOICEPI_HOTKEY_DRIVER").ok();
            if whisper_dictate_app::diag::should_warn_trace_needs_rdev(
                voicepi_log.as_deref(),
                driver_val.as_deref(),
            ) {
                whisper_dictate_app::diag::log!(
                    "[gui] VOICEPI_LOG=trace requested but VOICEPI_HOTKEY_DRIVER \
                     is unset or `register` - the boundary-trace decision tree \
                     relies on rdev's [rdev/callback] and [chord] lines which \
                     the RegisterHotKey backend does NOT emit. Set \
                     VOICEPI_HOTKEY_DRIVER=rdev explicitly before running the \
                     trace investigation to get accurate diagnostics."
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

    let exit = whisper_dictate_app::entrypoint::error_exit_shell(
        "error",
        std::io::stderr(),
        whisper_dictate_app::ui::run,
    );

    // Drain any pending async diagnostic records BEFORE main returns
    // — a bare return would kill the writer thread mid-drain and lose
    // whatever was still queued. Codex P2 #675 PRRT_kwDOSfNjQs6UbAiW:
    // the writer sink is where wedge repros land, so a completed
    // repro must be durable in the tee file before the process exits.
    // 500 ms is more than enough for the sub-millisecond backlogs the
    // queue normally carries, and short enough to bound teardown
    // latency if the writer thread is stuck (e.g. AppData volume
    // wedged during process exit). Best-effort — the return value is
    // logged but not acted on.
    let drained =
        whisper_dictate_app::diag_async::drain_and_shutdown(std::time::Duration::from_millis(500));
    if !drained {
        // Codex P2 #675 PRRT_kwDOSfNjQs6Ub__j: this warning MUST NOT go
        // through `diag::log!`. The most likely reason the drain missed
        // its deadline is that the writer thread is stuck inside
        // `diag::write_line` still HOLDING the tee-file mutex (a wedged
        // AppData volume is exactly the scenario the deadline exists
        // for). A blocking `log!` here would immediately queue on that
        // same mutex and hang the GUI indefinitely after the nominal
        // 500 ms budget. `write_line_nonblocking` `try_lock`s the tee
        // and falls back to stderr-only, so teardown always completes.
        whisper_dictate_app::diag::write_line_nonblocking(
            "[gui] diag async writer drain-and-shutdown deadline expired; \
             pending records may not have landed in the tee file",
        );
    }

    exit
}
