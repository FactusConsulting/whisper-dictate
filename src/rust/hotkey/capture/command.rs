//! CLI orchestration and explicit interactive shortcut persistence.

use super::actions::{
    build_capture_action_sink, capture_until_deadline, emit, focus_snapshot_for_process,
    CaptureLoopOptions, Counters,
};
use super::events::{parse_duration_secs, resolve_chord_key_names, CapturedChord};
use super::raw_tap::build_raw_tap;
use super::{CaptureEvent, OUTPUT_PREFIX};
use crate::cli::HotkeyCommand;
use crate::config::{config_path, load_settings_from_path, save_settings_to_path};
use crate::hotkey::coordinator::CoordinatorHandle;
use crate::hotkey::{install_hotkey_with_focus_snapshot, HotkeyConfig, InstallError};
use anyhow::{anyhow, Result};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

/// Entry point for the `hotkey` subcommand family.
pub fn handle_hotkey_command(cmd: HotkeyCommand) -> Result<()> {
    match cmd {
        HotkeyCommand::Capture {
            for_secs,
            json,
            chord_events_only,
            focus_process,
            exit_on_chord,
            configure,
            config,
            driver,
            chord,
        } => {
            let duration = parse_duration_secs(&for_secs)?;
            // Route the driver preference through the same env var the
            // shipping install path consults (`VOICEPI_HOTKEY_DRIVER`).
            // Rejecting unrecognised values BEFORE install matches the
            // rest of the CLI's fail-fast policy — a `--driver foo` typo
            // should not silently fall back to `auto`.
            validate_driver_flag(&driver)?;
            validate_configure_args(
                configure,
                json,
                chord_events_only,
                focus_process,
                exit_on_chord,
                &driver,
                chord.as_deref(),
            )?;
            std::env::set_var("VOICEPI_HOTKEY_DRIVER", driver);
            run_capture(
                RunCaptureOptions {
                    duration,
                    json,
                    chord_events_only,
                    focus_process,
                    exit_on_chord,
                    configure,
                },
                config.as_deref().map(Path::new),
                chord.as_deref(),
            )
        }
    }
}

pub(super) fn validate_configure_args(
    configure: bool,
    json: bool,
    chord_events_only: bool,
    focus_process: Option<u32>,
    exit_on_chord: bool,
    driver: &str,
    chord_override: Option<&str>,
) -> Result<()> {
    if configure && json {
        return Err(anyhow!(
            "--configure is interactive and cannot be combined with --json"
        ));
    }
    if configure && chord_events_only {
        return Err(anyhow!(
            "--configure needs raw key events and cannot be combined with --chord-events-only"
        ));
    }
    if focus_process.is_some() && !chord_events_only {
        return Err(anyhow!(
            "--focus-process is internal to --chord-events-only diagnostics"
        ));
    }
    if configure
        && cfg!(target_os = "windows")
        && matches!(
            driver.trim().to_ascii_lowercase().as_str(),
            "register" | "win_registerhotkey" | "wm_hotkey"
        )
    {
        return Err(anyhow!(
            "--configure needs a raw key-event listener; use --driver rdev or auto"
        ));
    }
    if configure && exit_on_chord {
        return Err(anyhow!(
            "--configure captures on release and cannot be combined with --exit-on-chord"
        ));
    }
    if configure && chord_override.is_some() {
        return Err(anyhow!(
            "--configure captures a new chord and cannot be combined with --chord"
        ));
    }
    Ok(())
}

/// Reject `--driver` values that the manager's [`crate::hotkey::manager::DriverKind::parse`]
/// would silently coerce to `Auto`. The runtime tolerates typos (falls back
/// to Auto) because the env var may be set from many sources; the CLI is
/// stricter so a smoke script that mis-spells the flag fails fast instead of
/// installing the wrong backend.
pub fn validate_driver_flag(raw: &str) -> Result<()> {
    // Reuse the manager's canonical name/alias set so the CLI accepts exactly
    // what the runtime does — extending one extends the other automatically.
    #[cfg(feature = "rust-hotkeys")]
    {
        if crate::hotkey::manager::DriverKind::parse(raw).is_none() {
            return Err(anyhow!(
                "--driver expects auto | rdev | evdev (or the x11 / wayland \
                 aliases); got {raw:?}"
            ));
        }
        Ok(())
    }
    #[cfg(not(feature = "rust-hotkeys"))]
    {
        // Stock build has no driver-selection logic — accept the canonical
        // names AND the Windows RegisterHotKey aliases the help text
        // advertises so the flag parses cleanly. Install will then fail
        // with `InstallError::Unsupported` below (the actionable
        // "rebuild with `--features rust-hotkeys`" error). If we did NOT
        // accept the register aliases here, a smoke script running
        // `hotkey capture --driver register` on a stock build would hit
        // "unrecognised value" instead of the actionable rebuild
        // message — different exit code, different debugging path.
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "" | "rdev" | "x11" | "evdev" | "wayland" | "register"
            | "win_registerhotkey" | "wm_hotkey" => Ok(()),
            other => Err(anyhow!(
                "--driver expects auto | rdev | evdev | register (or the x11 / \
                 wayland / win_registerhotkey / wm_hotkey aliases); got {other:?}"
            )),
        }
    }
}

struct RunCaptureOptions {
    duration: Duration,
    json: bool,
    chord_events_only: bool,
    focus_process: Option<u32>,
    exit_on_chord: bool,
    configure: bool,
}

fn run_capture(
    options: RunCaptureOptions,
    config_override: Option<&Path>,
    chord_override: Option<&str>,
) -> Result<()> {
    let RunCaptureOptions {
        duration,
        json,
        chord_events_only,
        focus_process,
        exit_on_chord,
        configure,
    } = options;
    let key_names = if configure {
        vec!["pause".to_owned()]
    } else {
        resolve_chord_key_names(chord_override, config_override)?
    };
    let display_chord = key_names.join("+");
    // The diagnostic has no transcription worker, so processing must complete
    // inline after each release; otherwise subsequent presses remain pending.
    let cfg = HotkeyConfig::hold_to_talk(key_names.clone()).with_auto_complete_processing(true);

    let counters = Arc::new(Counters::default());
    let (event_tx, event_rx) = mpsc::channel::<CaptureEvent>();

    // Raw tap runs on the rdev listener thread — count every key event and
    // forward as a KeyDown/KeyUp CaptureEvent.
    let raw_counters = Arc::clone(&counters);
    let raw_tx = event_tx.clone();
    let raw_start = Instant::now();
    let raw_tap = build_raw_tap(
        raw_counters,
        raw_tx,
        raw_start,
        key_names.clone(),
        !chord_events_only,
    );

    // Closing the coordinator loop. `Release` moves the coordinator to
    // `Stage::Processing(id)`, which it leaves ONLY on
    // `CoordinatorEvent::ProcessingFinished`. In the shipping runtime the
    // session sink sends that once transcription completes
    // (`dictate_run.rs` populates the same kind of slot). This diagnostic
    // has no session and never sent it, so the coordinator parked in
    // Processing after the very first chord release and silently swallowed
    // every later press -- the verb went deaf at the exact moment it was
    // supposed to be reporting.
    //
    // There is nothing to wait for here, so completion is immediate. The
    // handle only exists after `install_hotkey_with_focus_snapshot` returns, while
    // the sink must be built before it, hence the `OnceLock` -- the same
    // chicken-and-egg the production wiring solves the same way.
    let coord_slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());

    let action_sink = build_capture_action_sink(
        Arc::clone(&counters),
        event_tx.clone(),
        Arc::clone(&coord_slot),
        raw_start,
        exit_on_chord,
    );

    // Install the listener. If the feature isn't compiled in, surface an
    // actionable error rather than hanging on the timeout — the operator
    // needs to know they have to rebuild with `--features rust-hotkeys`.
    let focus_snapshot = focus_snapshot_for_process(focus_process);
    let handle = match install_hotkey_with_focus_snapshot(cfg, action_sink, raw_tap, move || {
        focus_snapshot()
    }) {
        Ok(h) => h,
        Err(InstallError::Unsupported) => {
            return Err(anyhow!(
                "hotkey capture requires the `rust-hotkeys` cargo feature; \
                 rebuild with `cargo build --features rust-hotkeys` (or set \
                 VOICEPI_HOTKEY_BACKEND=rust on an appropriately-built binary)"
            ));
        }
        Err(err @ InstallError::EmptyConfig) => return Err(err.into()),
        Err(err @ InstallError::UnsupportedKey(_)) => return Err(err.into()),
        // Another whisper-dictate process owns push-to-talk. The refusal
        // message already names the pid to quit and what it prevented
        // (`hotkey::ptt_lock`), so pass it through verbatim rather than
        // re-wrapping it -- this diagnostic verb's operator is exactly the
        // audience it was written for.
        Err(err @ InstallError::AlreadyHeld { .. }) => return Err(err.into()),
        Err(InstallError::ListenerStartup(msg)) => {
            return Err(anyhow!(
                "hotkey listener failed to start ({msg}); on Linux without an X \
                 display this is expected - retry from a user session, or \
                 use the evdev backend if you have `/dev/input/*` permissions"
            ));
        }
    };

    // Publish the handle so the action sink can complete the Processing
    // stage. Must happen before the first chord can fire; the coordinator
    // thread is already running, but it cannot emit StopAndTranscribe until
    // a chord is pressed and released, which needs a human or a driven
    // event -- and `set` is atomic either way.
    let _ = coord_slot.set(handle.coordinator_handle());

    let start = raw_start;
    let deadline = start + duration;
    let mut captured = CapturedChord::default();

    let stdout = io::stdout();
    let mut lock = stdout.lock();
    // Take the driver name from the live handle so it reflects the ACTUAL
    // backend the manager picked (rdev vs evdev), not a hardcoded default.
    // On a stock build (no `rust-hotkeys`) install fails above and this
    // branch is unreachable, so the "none" stub never surfaces here.
    let driver = handle.driver_name();
    emit(
        &CaptureEvent::ListenerInstalled {
            driver,
            chord: display_chord,
        },
        json,
        &mut lock,
    );

    // Receive events until the deadline or an action terminal event.
    let captured_chord = capture_until_deadline(
        CaptureLoopOptions {
            deadline,
            start,
            json,
            configure,
        },
        &counters,
        &event_rx,
        &mut captured,
        &mut lock,
    );

    // Explicit shutdown (Drop would also do it, but making it explicit keeps
    // the exit ordering unambiguous — we want the tap/sink to stop firing
    // BEFORE the counters are read for the summary line above… which we
    // already emitted, so this is just tidy).
    drop(lock);
    handle.shutdown();
    if configure {
        let Some(chord) = captured_chord else {
            return Err(anyhow!(
                "no supported shortcut was captured before the timeout"
            ));
        };
        save_captured_chord(&chord, config_override)?;
    }
    Ok(())
}

fn save_captured_chord(chord: &str, config_override: Option<&Path>) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    write!(
        &mut stdout,
        "{OUTPUT_PREFIX} captured {chord}. Save this shortcut? [y/N]: "
    )?;
    stdout.flush()?;
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let discard_empty_line = chord
        .split('+')
        .any(|token| token.trim().eq_ignore_ascii_case("enter"));
    if !read_save_confirmation(&mut stdin, &mut stdout, discard_empty_line)? {
        writeln!(&mut stdout, "{OUTPUT_PREFIX} shortcut not saved")?;
        return Ok(());
    }
    let path = config_override
        .map(Path::to_path_buf)
        .unwrap_or_else(config_path);
    let mut settings = load_settings_from_path(&path)?;
    settings.key = chord.to_owned();
    let saved = save_settings_to_path(&settings, &path)?;
    writeln!(
        &mut stdout,
        "{OUTPUT_PREFIX} shortcut saved to {}",
        saved.display()
    )?;
    Ok(())
}

pub(super) fn read_save_confirmation(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    mut discard_empty_line: bool,
) -> io::Result<bool> {
    loop {
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            return Ok(false);
        }
        let answer = answer.trim().to_ascii_lowercase();
        if answer.is_empty() {
            if discard_empty_line {
                discard_empty_line = false;
                continue;
            }
            return Ok(false);
        }
        match answer.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => {
                write!(writer, "{OUTPUT_PREFIX} please answer [y/N]: ")?;
                writer.flush()?;
            }
        }
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
