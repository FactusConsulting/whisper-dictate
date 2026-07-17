//! `whisper-dictate hotkey capture` — diagnostic CLI that installs the PTT
//! listener for a bounded window, prints every OS key event and every
//! chord-level lifecycle transition the coordinator emits, then exits.
//!
//! Serves three purposes:
//!
//! * debugging PTT wedges (`does the listener see my chord at all?`),
//! * verifying the hotkey install path works on the running platform, and
//! * headless smoke-testing that the listener installs without crashing
//!   (`--for 0.5` in the wayland-user-smoke script — see audit item 2).
//!
//! The plain-text output is line-oriented for grep-ability; `--json` switches
//! to JSONL so callers can pin against a stable schema. Both formats route
//! through the same [`CaptureEvent`] value type so the formatter is a pure
//! function — see [`format_plain`] / [`format_json`] and their unit tests.
//!
//! The command deliberately does NOT modify `runtime.rs` — it goes straight
//! to [`super::install_hotkey_with_raw_tap`], which is the same install
//! surface `runtime::maybe_install_rust_hotkey` uses under the hood. That
//! keeps the diagnostic and the shipping path in lockstep without a shim.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::cli::HotkeyCommand;
use crate::config::{load_settings, load_settings_from_path};

use super::coordinator::CoordinatorAction;
#[cfg(feature = "rust-hotkeys")]
use super::manager::RawKeyEvent;
use super::{install_hotkey_with_raw_tap, HotkeyConfig, InstallError};

/// Line-prefix used for the human-readable output. Kept as a constant so
/// callers (smoke scripts, grep-based assertions) can pin against it.
pub const OUTPUT_PREFIX: &str = "[hotkey-capture]";

/// One line of diagnostic output. The plain-text and JSON formatters both
/// consume this so their behaviour stays symmetric and unit-testable.
///
/// `t_secs` is the seconds-since-install timestamp — held on the enum
/// alongside the payload so the formatters can be pure functions of the
/// event alone (no ambient state).
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureEvent {
    /// Emitted once, immediately after the listener install succeeded.
    ListenerInstalled { driver: &'static str, chord: String },
    /// Raw OS keydown observed by the rdev driver. `name` is the normalised
    /// key name (`ctrl_l`, `f9`, `space`, ...) or `__rdev_<Variant>` for
    /// unmapped keys.
    KeyDown { t_secs: f64, name: String },
    /// Raw OS keyup, same naming as [`Self::KeyDown`].
    KeyUp { t_secs: f64, name: String },
    /// The tracker completed the configured chord (rising edge).
    ChordMatched { t_secs: f64, id: u64 },
    /// The tracker observed the chord release (falling edge).
    ChordReleased { t_secs: f64, id: u64 },
    /// The tracker cancelled the in-flight chord — either a foreign key
    /// joined the modifier(s) mid-recording (bare-modifier rule 2) or the
    /// coordinator was reset. Included so operators can tell "chord broke
    /// because of foreign key" apart from "chord broke because of release".
    ChordCanceled { t_secs: f64, id: u64 },
    /// The `--for SECONDS` window elapsed. Terminal event.
    DurationReached {
        t_secs: f64,
        events: u64,
        chords: u64,
        foreign_keys: u64,
    },
    /// The `--exit-on-chord` flag was set and the chord fired. Terminal
    /// event. Prints in place of [`Self::DurationReached`] when the
    /// early-exit path triggers.
    ExitOnChord {
        t_secs: f64,
        events: u64,
        chords: u64,
        foreign_keys: u64,
    },
}

impl CaptureEvent {
    /// Terminal events end the capture loop when produced. Used by the run
    /// loop to break out of the timeout-recv wait; keeping the check on the
    /// enum itself means new terminal variants stay honest without touching
    /// the loop.
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            CaptureEvent::DurationReached { .. } | CaptureEvent::ExitOnChord { .. }
        )
    }
}

/// Format a [`CaptureEvent`] as one line of human-readable output. Pure
/// function; unit-tested exhaustively so the operator-facing shape can be
/// pinned by tests without spawning a listener.
pub fn format_plain(event: &CaptureEvent) -> String {
    match event {
        CaptureEvent::ListenerInstalled { driver, chord } => {
            format!("{OUTPUT_PREFIX} listener installed (driver={driver}, chord={chord})")
        }
        CaptureEvent::KeyDown { t_secs, name } => {
            format!("{OUTPUT_PREFIX} {t_secs:.3}s {name} DOWN")
        }
        CaptureEvent::KeyUp { t_secs, name } => {
            format!("{OUTPUT_PREFIX} {t_secs:.3}s {name} UP")
        }
        CaptureEvent::ChordMatched { t_secs, id } => {
            format!("{OUTPUT_PREFIX} {t_secs:.3}s CHORD MATCHED (id={id})")
        }
        CaptureEvent::ChordReleased { t_secs, id } => {
            format!("{OUTPUT_PREFIX} {t_secs:.3}s CHORD RELEASED (id={id})")
        }
        CaptureEvent::ChordCanceled { t_secs, id } => {
            format!("{OUTPUT_PREFIX} {t_secs:.3}s CHORD CANCELED (id={id})")
        }
        CaptureEvent::DurationReached {
            t_secs,
            events,
            chords,
            foreign_keys,
        } => format!(
            "{OUTPUT_PREFIX} {t_secs:.3}s duration reached, exiting\n  \
             Events: {events}  Chords: {chords}  Foreign keys: {foreign_keys}"
        ),
        CaptureEvent::ExitOnChord {
            t_secs,
            events,
            chords,
            foreign_keys,
        } => format!(
            "{OUTPUT_PREFIX} {t_secs:.3}s exit-on-chord fired, exiting\n  \
             Events: {events}  Chords: {chords}  Foreign keys: {foreign_keys}"
        ),
    }
}

/// Format a [`CaptureEvent`] as a single JSON object (JSONL). Pure function;
/// the produced JSON is the machine-readable contract callers should pin
/// against — the plain-text format is stable-ish but the JSON keys are
/// promised.
pub fn format_json(event: &CaptureEvent) -> String {
    let value = match event {
        CaptureEvent::ListenerInstalled { driver, chord } => json!({
            "kind": "listener_installed",
            "driver": driver,
            "chord": chord,
        }),
        CaptureEvent::KeyDown { t_secs, name } => json!({
            "t": round3(*t_secs),
            "kind": "key_down",
            "name": name,
        }),
        CaptureEvent::KeyUp { t_secs, name } => json!({
            "t": round3(*t_secs),
            "kind": "key_up",
            "name": name,
        }),
        CaptureEvent::ChordMatched { t_secs, id } => json!({
            "t": round3(*t_secs),
            "kind": "chord_matched",
            "id": id,
        }),
        CaptureEvent::ChordReleased { t_secs, id } => json!({
            "t": round3(*t_secs),
            "kind": "chord_released",
            "id": id,
        }),
        CaptureEvent::ChordCanceled { t_secs, id } => json!({
            "t": round3(*t_secs),
            "kind": "chord_canceled",
            "id": id,
        }),
        CaptureEvent::DurationReached {
            t_secs,
            events,
            chords,
            foreign_keys,
        } => json!({
            "t": round3(*t_secs),
            "kind": "duration_reached",
            "events": events,
            "chords": chords,
            "foreign_keys": foreign_keys,
        }),
        CaptureEvent::ExitOnChord {
            t_secs,
            events,
            chords,
            foreign_keys,
        } => json!({
            "t": round3(*t_secs),
            "kind": "exit_on_chord",
            "events": events,
            "chords": chords,
            "foreign_keys": foreign_keys,
        }),
    };
    value.to_string()
}

/// Round to 3 decimal places so the JSON `t` field renders as
/// `0.123` rather than `0.12300000000000001` and roundtrips cleanly through
/// callers that assert on exact strings.
fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Parse the `--for SECONDS` flag. Kept as a String on the enum for `Eq`
/// derivability; this helper is where we validate: numeric, finite, positive,
/// and capped at 24 h so a typo can't wedge the diagnostic.
pub(crate) fn parse_duration_secs(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    let secs: f64 = trimmed
        .parse()
        .map_err(|_| anyhow!("--for expects a numeric SECONDS value (got {trimmed:?})"))?;
    if !secs.is_finite() || secs <= 0.0 {
        return Err(anyhow!(
            "--for must be a positive finite number of seconds (got {secs})"
        ));
    }
    let capped = secs.min(24.0 * 3600.0);
    Ok(Duration::from_secs_f64(capped))
}

/// Split the PTT `settings.key` string into individual key names, trimming
/// whitespace and dropping empty segments. Mirrors the runtime.rs helper
/// (`extract_hotkey_key_names`) so the diagnostic and the shipping path
/// interpret the same config identically.
pub(crate) fn split_key_names(chord: &str) -> Vec<String> {
    chord
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether an early-exit condition was requested; the [`CaptureEvent::ExitOnChord`]
/// terminal event should be emitted on the first chord match if so.
///
/// Kept as a bool + a decision helper (rather than an enum) so future flags
/// (e.g. `--exit-after-N`) can layer in without rewiring the enum.
fn decide_terminal(
    action: &CoordinatorAction,
    exit_on_chord: bool,
    counters: &Counters,
    start: Instant,
) -> Option<CaptureEvent> {
    if !exit_on_chord {
        return None;
    }
    if matches!(action, CoordinatorAction::StartRecording(_)) {
        return Some(CaptureEvent::ExitOnChord {
            t_secs: start.elapsed().as_secs_f64(),
            events: counters.events.load(Ordering::Relaxed),
            chords: counters.chords.load(Ordering::Relaxed),
            foreign_keys: counters.foreign_keys.load(Ordering::Relaxed),
        });
    }
    None
}

/// Shared, thread-safe counters incremented from the rdev listener thread
/// (raw tap) and the coordinator thread (action sink). Read on the main
/// thread when the terminal event fires.
#[derive(Default)]
struct Counters {
    events: AtomicU64,
    chords: AtomicU64,
    foreign_keys: AtomicU64,
}

/// Entry point for the `hotkey` subcommand family.
pub fn handle_hotkey_command(cmd: HotkeyCommand) -> Result<()> {
    match cmd {
        HotkeyCommand::Capture {
            for_secs,
            json,
            exit_on_chord,
            config,
            driver,
        } => {
            let duration = parse_duration_secs(&for_secs)?;
            // Route the driver preference through the same env var the
            // shipping install path consults (`VOICEPI_HOTKEY_DRIVER`).
            // Rejecting unrecognised values BEFORE install matches the
            // rest of the CLI's fail-fast policy — a `--driver foo` typo
            // should not silently fall back to `auto`.
            validate_driver_flag(&driver)?;
            std::env::set_var("VOICEPI_HOTKEY_DRIVER", driver);
            run_capture(
                duration,
                json,
                exit_on_chord,
                config.as_deref().map(Path::new),
            )
        }
    }
}

/// Reject `--driver` values that the manager's [`crate::hotkey::manager::DriverKind::parse`]
/// would silently coerce to `Auto`. The runtime tolerates typos (falls back
/// to Auto) because the env var may be set from many sources; the CLI is
/// stricter so a smoke script that mis-spells the flag fails fast instead of
/// installing the wrong backend.
pub(crate) fn validate_driver_flag(raw: &str) -> Result<()> {
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
        // names so the flag parses cleanly, but install will fail with
        // `InstallError::Unsupported` below.
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "" | "rdev" | "x11" | "evdev" | "wayland" => Ok(()),
            other => Err(anyhow!(
                "--driver expects auto | rdev | evdev (or the x11 / wayland \
                 aliases); got {other:?}"
            )),
        }
    }
}

/// Emit `event` on stdout with the requested format, flushing so the
/// buffered writer doesn't hold events past a Ctrl-C.
fn emit(event: &CaptureEvent, json: bool, stdout: &mut io::StdoutLock<'_>) {
    let line = if json {
        format_json(event)
    } else {
        format_plain(event)
    };
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn run_capture(
    duration: Duration,
    json: bool,
    exit_on_chord: bool,
    config_override: Option<&Path>,
) -> Result<()> {
    let settings = match config_override {
        Some(p) => load_settings_from_path(p)?,
        None => load_settings()?,
    };
    let chord_str = settings.key.trim().to_owned();
    let key_names = split_key_names(&chord_str);
    if key_names.is_empty() {
        return Err(anyhow!(
            "no PTT chord configured (settings.key is empty in the resolved config)"
        ));
    }
    let display_chord = key_names.join("+");
    let cfg = HotkeyConfig::hold_to_talk(key_names);

    let counters = Arc::new(Counters::default());
    let (event_tx, event_rx) = mpsc::channel::<CaptureEvent>();

    // Raw tap runs on the rdev listener thread — count every key event and
    // forward as a KeyDown/KeyUp CaptureEvent.
    let raw_counters = Arc::clone(&counters);
    let raw_tx = event_tx.clone();
    let raw_start = Instant::now();
    let raw_tap = build_raw_tap(raw_counters, raw_tx, raw_start);

    // Action sink runs on the coordinator thread — chord lifecycle events.
    let action_counters = Arc::clone(&counters);
    let action_tx = event_tx.clone();
    let action_start = raw_start;
    let action_sink = move |action: CoordinatorAction| {
        action_counters.chords.fetch_add(1, Ordering::Relaxed);
        let now = action_start.elapsed().as_secs_f64();
        let event = match action {
            CoordinatorAction::StartRecording(id) => CaptureEvent::ChordMatched { t_secs: now, id },
            CoordinatorAction::StopAndTranscribe(id) => {
                CaptureEvent::ChordReleased { t_secs: now, id }
            }
            CoordinatorAction::CancelRecording(id) => {
                CaptureEvent::ChordCanceled { t_secs: now, id }
            }
        };
        let _ = action_tx.send(event);
        if let Some(terminal) =
            decide_terminal(&action, exit_on_chord, &action_counters, action_start)
        {
            let _ = action_tx.send(terminal);
        }
    };

    // Install the listener. If the feature isn't compiled in, surface an
    // actionable error rather than hanging on the timeout — the operator
    // needs to know they have to rebuild with `--features rust-hotkeys`.
    let handle = match install_hotkey_with_raw_tap(cfg, action_sink, raw_tap) {
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
        Err(InstallError::ListenerStartup(msg)) => {
            return Err(anyhow!(
                "hotkey listener failed to start ({msg}); on Linux without an X \
                 display this is expected — retry from a user session, or \
                 use the evdev backend if you have `/dev/input/*` permissions"
            ));
        }
    };

    let start = raw_start;
    let deadline = start + duration;

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

    // Main loop — recv events until either the deadline expires or the
    // action sink signals an early exit via `terminated`.
    loop {
        let now = Instant::now();
        if now >= deadline {
            let elapsed = start.elapsed().as_secs_f64();
            let terminal = CaptureEvent::DurationReached {
                t_secs: elapsed,
                events: counters.events.load(Ordering::Relaxed),
                chords: counters.chords.load(Ordering::Relaxed),
                foreign_keys: counters.foreign_keys.load(Ordering::Relaxed),
            };
            emit(&terminal, json, &mut lock);
            break;
        }
        // Poll with the remaining budget so we wake on the deadline exactly.
        let remaining = deadline.saturating_duration_since(now);
        match event_rx.recv_timeout(remaining) {
            Ok(event) => {
                emit(&event, json, &mut lock);
                if event.is_terminal() {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // loop head handles the terminal emission
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Every producer went away — treat as end-of-stream. Should
                // never happen while `handle` is alive, but be defensive.
                break;
            }
        }
    }

    // Explicit shutdown (Drop would also do it, but making it explicit keeps
    // the exit ordering unambiguous — we want the tap/sink to stop firing
    // BEFORE the counters are read for the summary line above… which we
    // already emitted, so this is just tidy).
    handle.shutdown();
    Ok(())
}

// `driver_name` was previously a hard-coded `"rdev"` because the CLI only
// wired up that backend. The evdev listener (audit item 5 prereq 2) now
// makes the choice a runtime decision — read via `HotkeyHandle::driver_name()`
// right after `install_hotkey_with_raw_tap` returns instead.

/// Build the raw-event tap the manager thread invokes for every OS key
/// event. Isolated into its own helper so the closure has a well-defined
/// capture set — makes the borrow-checker happy and keeps run_capture
/// readable.
#[cfg(feature = "rust-hotkeys")]
fn build_raw_tap(
    counters: Arc<Counters>,
    tx: Sender<CaptureEvent>,
    start: Instant,
) -> impl super::manager::RawTap {
    move |raw: &RawKeyEvent| {
        counters.events.fetch_add(1, Ordering::Relaxed);
        let t_secs = start.elapsed().as_secs_f64();
        let event = match raw.kind {
            super::manager::RawKeyKind::Press => CaptureEvent::KeyDown {
                t_secs,
                name: raw.name.clone(),
            },
            super::manager::RawKeyKind::Release => CaptureEvent::KeyUp {
                t_secs,
                name: raw.name.clone(),
            },
        };
        let _ = tx.send(event);
    }
}

/// Non-feature build: the tap is never invoked (install returns Unsupported
/// before threads spawn), so return a zero-cost noop. Kept here so
/// `run_capture` compiles under both feature configurations.
#[cfg(not(feature = "rust-hotkeys"))]
#[allow(clippy::unused_unit)]
fn build_raw_tap(
    _counters: Arc<Counters>,
    _tx: Sender<CaptureEvent>,
    _start: Instant,
) -> impl Send + Sync + 'static {
    // `()` implements Send + Sync + 'static and satisfies the stock-build
    // `install_hotkey_with_raw_tap` bound. It is never invoked — the stock
    // install returns Unsupported before any listener thread spawns.
    ()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_duration_secs
    // -----------------------------------------------------------------------

    #[test]
    fn parse_duration_accepts_integer_seconds() {
        let d = parse_duration_secs("5").unwrap();
        assert_eq!(d, Duration::from_secs(5));
    }

    #[test]
    fn parse_duration_accepts_fractional_seconds() {
        let d = parse_duration_secs("0.5").unwrap();
        assert_eq!(d, Duration::from_millis(500));
    }

    #[test]
    fn parse_duration_trims_whitespace() {
        let d = parse_duration_secs("  0.25 ").unwrap();
        assert_eq!(d, Duration::from_millis(250));
    }

    #[test]
    fn parse_duration_rejects_non_numeric() {
        let err = parse_duration_secs("foo").unwrap_err().to_string();
        assert!(err.contains("numeric"), "unexpected error: {err}");
    }

    #[test]
    fn parse_duration_rejects_zero_and_negative() {
        assert!(parse_duration_secs("0").is_err());
        assert!(parse_duration_secs("-1").is_err());
        assert!(parse_duration_secs("-0.5").is_err());
    }

    #[test]
    fn parse_duration_rejects_non_finite() {
        assert!(parse_duration_secs("inf").is_err());
        assert!(parse_duration_secs("NaN").is_err());
    }

    #[test]
    fn parse_duration_caps_at_24_hours() {
        // A typo like `--for 999999` shouldn't wedge the tool overnight.
        let d = parse_duration_secs("999999").unwrap();
        assert_eq!(d, Duration::from_secs(24 * 3600));
    }

    // -----------------------------------------------------------------------
    // split_key_names — mirrors runtime::extract_hotkey_key_names behaviour
    // -----------------------------------------------------------------------

    #[test]
    fn split_key_names_single_key() {
        assert_eq!(split_key_names("ctrl_r"), vec!["ctrl_r".to_owned()]);
    }

    #[test]
    fn split_key_names_multi_key_chord() {
        assert_eq!(
            split_key_names("ctrl_l+shift_l+l"),
            vec!["ctrl_l".to_owned(), "shift_l".to_owned(), "l".to_owned(),]
        );
    }

    #[test]
    fn split_key_names_trims_and_drops_empty() {
        assert_eq!(
            split_key_names("  ctrl_l +  + shift_r "),
            vec!["ctrl_l".to_owned(), "shift_r".to_owned()]
        );
    }

    #[test]
    fn split_key_names_empty_input_yields_empty_vec() {
        assert!(split_key_names("").is_empty());
        assert!(split_key_names("   ").is_empty());
        assert!(split_key_names("+ + +").is_empty());
    }

    // -----------------------------------------------------------------------
    // format_plain
    // -----------------------------------------------------------------------

    #[test]
    fn plain_install_line_has_driver_and_chord() {
        let line = format_plain(&CaptureEvent::ListenerInstalled {
            driver: "rdev",
            chord: "ctrl_l+shift_l+l".to_owned(),
        });
        assert!(line.starts_with(OUTPUT_PREFIX), "prefix: {line}");
        assert!(line.contains("driver=rdev"));
        assert!(line.contains("chord=ctrl_l+shift_l+l"));
    }

    #[test]
    fn plain_key_events_include_timestamp_and_name() {
        let down = format_plain(&CaptureEvent::KeyDown {
            t_secs: 0.123,
            name: "ctrl_l".to_owned(),
        });
        assert!(down.contains("0.123s"), "line: {down}");
        assert!(down.contains("ctrl_l DOWN"));
        let up = format_plain(&CaptureEvent::KeyUp {
            t_secs: 1.5,
            name: "shift_r".to_owned(),
        });
        assert!(up.contains("1.500s"));
        assert!(up.contains("shift_r UP"));
    }

    #[test]
    fn plain_chord_events_report_matched_released_canceled() {
        let matched = format_plain(&CaptureEvent::ChordMatched { t_secs: 0.1, id: 7 });
        let released = format_plain(&CaptureEvent::ChordReleased { t_secs: 0.5, id: 7 });
        let canceled = format_plain(&CaptureEvent::ChordCanceled { t_secs: 0.6, id: 8 });
        assert!(matched.contains("CHORD MATCHED"));
        assert!(released.contains("CHORD RELEASED"));
        assert!(canceled.contains("CHORD CANCELED"));
        // The id is exposed so operators can pair matched with released.
        assert!(matched.contains("id=7"));
        assert!(released.contains("id=7"));
        assert!(canceled.contains("id=8"));
    }

    #[test]
    fn plain_duration_reached_includes_summary_counters() {
        let line = format_plain(&CaptureEvent::DurationReached {
            t_secs: 5.0,
            events: 12,
            chords: 3,
            foreign_keys: 1,
        });
        assert!(line.contains("duration reached"));
        assert!(line.contains("Events: 12"));
        assert!(line.contains("Chords: 3"));
        assert!(line.contains("Foreign keys: 1"));
    }

    #[test]
    fn plain_exit_on_chord_includes_summary_counters() {
        let line = format_plain(&CaptureEvent::ExitOnChord {
            t_secs: 0.2,
            events: 3,
            chords: 1,
            foreign_keys: 0,
        });
        assert!(line.contains("exit-on-chord"));
        assert!(line.contains("Events: 3"));
        assert!(line.contains("Chords: 1"));
    }

    // -----------------------------------------------------------------------
    // format_json
    // -----------------------------------------------------------------------

    fn parse_json(line: &str) -> serde_json::Value {
        serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSON {line:?}: {e}"))
    }

    #[test]
    fn json_install_line_has_kind_driver_chord() {
        let v = parse_json(&format_json(&CaptureEvent::ListenerInstalled {
            driver: "rdev",
            chord: "ctrl_r".to_owned(),
        }));
        assert_eq!(v["kind"], "listener_installed");
        assert_eq!(v["driver"], "rdev");
        assert_eq!(v["chord"], "ctrl_r");
    }

    #[test]
    fn json_key_events_have_kind_t_name() {
        let down = parse_json(&format_json(&CaptureEvent::KeyDown {
            t_secs: 0.123,
            name: "ctrl_l".to_owned(),
        }));
        assert_eq!(down["kind"], "key_down");
        assert_eq!(down["name"], "ctrl_l");
        assert_eq!(down["t"], 0.123);

        let up = parse_json(&format_json(&CaptureEvent::KeyUp {
            t_secs: 0.145,
            name: "shift_l".to_owned(),
        }));
        assert_eq!(up["kind"], "key_up");
        assert_eq!(up["name"], "shift_l");
    }

    #[test]
    fn json_chord_events_have_id_and_kind() {
        let matched = parse_json(&format_json(&CaptureEvent::ChordMatched {
            t_secs: 0.167,
            id: 42,
        }));
        assert_eq!(matched["kind"], "chord_matched");
        assert_eq!(matched["id"], 42);
        let released = parse_json(&format_json(&CaptureEvent::ChordReleased {
            t_secs: 0.412,
            id: 42,
        }));
        assert_eq!(released["kind"], "chord_released");
        let canceled = parse_json(&format_json(&CaptureEvent::ChordCanceled {
            t_secs: 0.5,
            id: 43,
        }));
        assert_eq!(canceled["kind"], "chord_canceled");
    }

    #[test]
    fn json_terminal_events_carry_counters() {
        let dur = parse_json(&format_json(&CaptureEvent::DurationReached {
            t_secs: 5.0,
            events: 7,
            chords: 1,
            foreign_keys: 0,
        }));
        assert_eq!(dur["kind"], "duration_reached");
        assert_eq!(dur["events"], 7);
        assert_eq!(dur["chords"], 1);
        assert_eq!(dur["foreign_keys"], 0);

        let onchord = parse_json(&format_json(&CaptureEvent::ExitOnChord {
            t_secs: 0.3,
            events: 3,
            chords: 1,
            foreign_keys: 0,
        }));
        assert_eq!(onchord["kind"], "exit_on_chord");
        assert_eq!(onchord["chords"], 1);
    }

    #[test]
    fn json_t_field_is_rounded_to_three_decimals() {
        // Guards against `0.12300000000000001` sneaking into the machine-
        // readable output — tests that pin the JSON contract would break
        // otherwise.
        let line = format_json(&CaptureEvent::KeyDown {
            t_secs: 0.1230000000001,
            name: "a".to_owned(),
        });
        assert!(line.contains("\"t\":0.123"), "unexpected: {line}");
    }

    // -----------------------------------------------------------------------
    // is_terminal
    // -----------------------------------------------------------------------

    #[test]
    fn only_duration_and_exit_on_chord_are_terminal() {
        let inst = CaptureEvent::ListenerInstalled {
            driver: "rdev",
            chord: "ctrl_r".to_owned(),
        };
        assert!(!inst.is_terminal());
        assert!(!CaptureEvent::KeyDown {
            t_secs: 0.0,
            name: "a".to_owned(),
        }
        .is_terminal());
        assert!(!CaptureEvent::ChordMatched { t_secs: 0.0, id: 1 }.is_terminal());
        assert!(CaptureEvent::DurationReached {
            t_secs: 5.0,
            events: 0,
            chords: 0,
            foreign_keys: 0,
        }
        .is_terminal());
        assert!(CaptureEvent::ExitOnChord {
            t_secs: 0.1,
            events: 0,
            chords: 0,
            foreign_keys: 0,
        }
        .is_terminal());
    }

    // -----------------------------------------------------------------------
    // decide_terminal — the exit-on-chord condition
    // -----------------------------------------------------------------------

    #[test]
    fn decide_terminal_no_early_exit_when_flag_off() {
        let counters = Counters::default();
        let start = Instant::now();
        let action =
            CoordinatorAction::StartRecording(super::super::coordinator::RecordingId::from(1u8));
        assert!(decide_terminal(&action, false, &counters, start).is_none());
    }

    #[test]
    fn decide_terminal_fires_on_start_recording_when_flag_on() {
        let counters = Counters::default();
        counters.events.store(4, Ordering::Relaxed);
        counters.chords.store(1, Ordering::Relaxed);
        let start = Instant::now();
        let action =
            CoordinatorAction::StartRecording(super::super::coordinator::RecordingId::from(1u8));
        let term = decide_terminal(&action, true, &counters, start).expect("terminal");
        match term {
            CaptureEvent::ExitOnChord { events, chords, .. } => {
                assert_eq!(events, 4);
                assert_eq!(chords, 1);
            }
            other => panic!("expected ExitOnChord, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // validate_driver_flag — CLI-level driver selection (audit item 5 prereq 2)
    // -----------------------------------------------------------------------

    #[test]
    fn validate_driver_flag_accepts_canonical_names() {
        // The three canonical values map to the runtime's DriverKind enum.
        assert!(validate_driver_flag("auto").is_ok());
        assert!(validate_driver_flag("rdev").is_ok());
        assert!(validate_driver_flag("evdev").is_ok());
    }

    #[test]
    fn validate_driver_flag_accepts_session_aliases() {
        // Session-name aliases mirror the manager's `DriverKind::parse`
        // acceptance set so a user can type the display server name.
        assert!(validate_driver_flag("x11").is_ok());
        assert!(validate_driver_flag("wayland").is_ok());
    }

    #[test]
    fn validate_driver_flag_is_case_insensitive() {
        assert!(validate_driver_flag("AUTO").is_ok());
        assert!(validate_driver_flag(" Evdev ").is_ok());
    }

    #[test]
    fn validate_driver_flag_rejects_typos_up_front() {
        // CLI-side strictness: unlike the env-var path, `--driver` must
        // fail-fast on a typo so a smoke script sees a clear error instead
        // of silently installing the auto-selected backend.
        let err = validate_driver_flag("uinput").unwrap_err().to_string();
        assert!(
            err.contains("--driver"),
            "error should mention --driver: {err}"
        );
        assert!(
            err.contains("uinput"),
            "error should echo the bad value: {err}"
        );
    }

    #[test]
    fn decide_terminal_ignores_release_and_cancel_actions() {
        // Only the *matched* rising edge triggers exit-on-chord — a release
        // or cancel arriving before a start would exit prematurely.
        let counters = Counters::default();
        let start = Instant::now();
        let release =
            CoordinatorAction::StopAndTranscribe(super::super::coordinator::RecordingId::from(1u8));
        let cancel =
            CoordinatorAction::CancelRecording(super::super::coordinator::RecordingId::from(1u8));
        assert!(decide_terminal(&release, true, &counters, start).is_none());
        assert!(decide_terminal(&cancel, true, &counters, start).is_none());
    }
}
