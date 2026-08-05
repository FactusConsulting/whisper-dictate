//! `wd hotkey capture` — diagnostic CLI that installs the PTT
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
//! surface the native runtime uses under the hood. That
//! keeps the diagnostic and the shipping path in lockstep without a shim.

use std::collections::BTreeSet;
#[cfg(feature = "rust-hotkeys")]
use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
#[cfg(feature = "rust-hotkeys")]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::cli::HotkeyCommand;
use crate::config::{config_path, load_settings, load_settings_from_path, save_settings_to_path};

use super::coordinator::{CoordinatorAction, CoordinatorEvent, CoordinatorHandle, RecordingId};
#[cfg(feature = "rust-hotkeys")]
use super::manager::{is_chord_key, RawKeyEvent};
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

#[derive(Default)]
struct CapturedChord {
    held: BTreeSet<String>,
    seen: BTreeSet<String>,
    invalid: bool,
    changed_after_release: bool,
}

impl CapturedChord {
    fn observe(&mut self, event: &CaptureEvent) -> Option<String> {
        let (raw_name, pressed) = match event {
            CaptureEvent::KeyDown { name, .. } => (name, true),
            CaptureEvent::KeyUp { name, .. } => (name, false),
            _ => return None,
        };
        let raw_name = raw_name.trim().to_ascii_lowercase();
        let Some(name) = capture_key_name(&raw_name) else {
            self.invalid = true;
            self.seen.clear();
            if pressed {
                self.held.insert(raw_name);
            } else {
                self.held.remove(&raw_name);
            }
            if self.held.is_empty() {
                self.invalid = false;
                self.changed_after_release = false;
            }
            return None;
        };
        if pressed {
            if self.invalid {
                self.held.insert(name);
                return None;
            }
            if self.changed_after_release {
                self.invalid = true;
                self.seen.clear();
                self.held.insert(name);
                return None;
            }
            self.seen.insert(name.clone());
            self.held.insert(name);
            return None;
        }
        self.held.remove(&name);
        if self.invalid {
            if self.held.is_empty() {
                self.invalid = false;
                self.changed_after_release = false;
            }
            return None;
        }
        if !self.held.is_empty() {
            self.changed_after_release = true;
            return None;
        }
        if self.held.is_empty() && !self.seen.is_empty() {
            let chord = format_captured_chord(&self.seen);
            self.held.clear();
            self.seen.clear();
            self.changed_after_release = false;
            return Some(chord);
        }
        None
    }
}

fn capture_key_name(name: &str) -> Option<String> {
    let name = name.trim().to_ascii_lowercase();
    let is_function = name
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=12).contains(&n));
    let is_named = matches!(name.as_str(), "pause" | "space" | "esc" | "tab" | "enter");
    if !is_function && !is_named && crate::hotkey::modifier_match::modifier_family(&name).is_none()
    {
        return None;
    }
    Some(crate::hotkey::modifier_match::canonical_side(&name).to_owned())
}

fn format_captured_chord(keys: &BTreeSet<String>) -> String {
    let mut ordered: Vec<&str> = keys.iter().map(String::as_str).collect();
    ordered.sort_by_key(|key| {
        crate::hotkey::modifier_match::modifier_family(key)
            .map(|family| match family {
                "ctrl" => 0,
                "shift" => 1,
                "alt" => 2,
                "cmd" => 3,
                _ => 4,
            })
            .unwrap_or(10)
    });
    ordered.join("+")
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

/// Resolve the PTT chord names to install the listener against, honouring
/// the `--chord` override.
///
/// Precedence is `--chord` > `--config` > default config resolution
/// (`VOICEPI_CONFIG` env / platform user config). `--chord` deliberately
/// short-circuits the config read entirely -- the whole point of the flag
/// is to test a chord WITHOUT touching (or being blocked by) whatever the
/// user has saved. An empty `settings.key` is a hard error in the config
/// path, and that must not stop someone from verifying a fresh chord.
///
/// Factored out of [`run_capture`] so precedence and coordinator input can be
/// tested without starting an operating-system listener thread.
pub(crate) fn resolve_chord_key_names(
    chord_override: Option<&str>,
    config_override: Option<&Path>,
) -> Result<Vec<String>> {
    let chord_str = match chord_override {
        Some(raw) => raw.trim().to_owned(),
        None => {
            let settings = match config_override {
                Some(p) => load_settings_from_path(p)?,
                None => load_settings()?,
            };
            settings.key.trim().to_owned()
        }
    };
    let key_names = split_key_names(&chord_str);
    if key_names.is_empty() {
        return Err(match chord_override {
            Some(raw) => anyhow!(
                "--chord {raw:?} contains no key names; expected `+`-separated \
                 names like `ctrl_l` or `shift_r+f9`"
            ),
            None => {
                anyhow!("no PTT chord configured (settings.key is empty in the resolved config)")
            }
        });
    }
    Ok(key_names)
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
            validate_configure_args(configure, json, exit_on_chord, &driver, chord.as_deref())?;
            std::env::set_var("VOICEPI_HOTKEY_DRIVER", driver);
            run_capture(
                duration,
                json,
                exit_on_chord,
                config.as_deref().map(Path::new),
                chord.as_deref(),
                configure,
            )
        }
    }
}

fn validate_configure_args(
    configure: bool,
    json: bool,
    exit_on_chord: bool,
    driver: &str,
    chord_override: Option<&str>,
) -> Result<()> {
    if configure && json {
        return Err(anyhow!(
            "--configure is interactive and cannot be combined with --json"
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
    chord_override: Option<&str>,
    configure: bool,
) -> Result<()> {
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
    let raw_tap = build_raw_tap(raw_counters, raw_tx, raw_start, key_names.clone());

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
    // handle only exists after `install_hotkey_with_raw_tap` returns, while
    // the sink must be built before it, hence the `OnceLock` -- the same
    // chicken-and-egg the production wiring solves the same way.
    let coord_slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());

    // Action sink runs on the coordinator thread — chord lifecycle events.
    let action_counters = Arc::clone(&counters);
    let action_tx = event_tx.clone();
    let action_start = raw_start;
    let action_coord = Arc::clone(&coord_slot);
    let action_sink = move |action: CoordinatorAction| {
        // Count one chord for each start action; release and cancel complete
        // that same chord and must not increment the counter again.
        if counts_as_chord(&action) {
            action_counters.chords.fetch_add(1, Ordering::Relaxed);
        }
        let now = action_start.elapsed().as_secs_f64();
        let event = match action {
            CoordinatorAction::StartRecording(id) => CaptureEvent::ChordMatched { t_secs: now, id },
            CoordinatorAction::StopAndTranscribe(id) => {
                complete_processing_stage(action_coord.get(), id);
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
    let mut captured_chord = None;

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
                if configure {
                    if let Some(chord) = captured.observe(&event) {
                        captured_chord = Some(chord);
                        break;
                    }
                }
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
    if !read_save_confirmation(&mut stdin, &mut stdout)? {
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

fn read_save_confirmation(reader: &mut impl BufRead, writer: &mut impl Write) -> io::Result<bool> {
    loop {
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            return Ok(false);
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => {
                write!(writer, "{OUTPUT_PREFIX} please answer [y/N]: ")?;
                writer.flush()?;
            }
        }
    }
}

// `driver_name` was previously a hard-coded `"rdev"` because the CLI only
// wired up that backend. The evdev listener (audit item 5 prereq 2) now
// makes the choice a runtime decision — read via `HotkeyHandle::driver_name()`
// right after `install_hotkey_with_raw_tap` returns instead.

/// Should this coordinator action increment the `Chords:` counter?
///
/// The coordinator emits three actions per chord (`StartRecording` on the
/// rising edge, then `StopAndTranscribe` or `CancelRecording` on release), so
/// counting every action double-reports — a two-chord capture showed
/// `"chords":4`. Only the rising edge counts as one chord fire.
///
/// Extracted from the `action_sink` closure so the producing side of the fix
/// can be unit-tested (`chords_counter_*`) directly. Otherwise only the
/// formatter side of the counter was covered — the same failure mode this
/// PR set out to fix for `foreign_keys`.
fn counts_as_chord(action: &CoordinatorAction) -> bool {
    matches!(action, CoordinatorAction::StartRecording(_))
}

/// Build the raw-event tap the manager thread invokes for every OS key
/// event. Isolated into its own helper so the closure has a well-defined
/// capture set — makes the borrow-checker happy and keeps run_capture
/// readable.
/// Hand the coordinator back to Idle after a stop.
///
/// `Release` moves the coordinator to `Stage::Processing(id)`, which it
/// leaves ONLY on a matching `ProcessingFinished`. The shipping runtime sends
/// that when transcription completes; this diagnostic has no session, so
/// completion is immediate and unconditional.
///
/// A `None` handle is a no-op rather than an error: the slot is populated
/// right after install, and the only window where it is empty is before any
/// chord can have fired.
fn complete_processing_stage(handle: Option<&CoordinatorHandle>, id: RecordingId) {
    if let Some(handle) = handle {
        handle.send(CoordinatorEvent::ProcessingFinished(id));
    }
}

#[cfg(feature = "rust-hotkeys")]
fn build_raw_tap(
    counters: Arc<Counters>,
    tx: Sender<CaptureEvent>,
    start: Instant,
    targets: Vec<String>,
) -> impl super::manager::RawTap {
    // Foreign keys are counted as PHYSICAL presses, not raw key-down events.
    // Holding a non-chord key produces a stream of auto-repeat key-downs (the
    // maintainer's capture logged ~20 for a single held Shift), so counting
    // events would report 20 foreign keys for one keypress. Track which
    // foreign keys are currently down and count only the not-held -> held
    // transition.
    //
    // `RawTap` is `Fn + Send + Sync`, so the held-set needs interior
    // mutability; the lock is uncontended (one listener thread) and held for
    // a single set operation.
    let held_foreign: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    move |raw: &RawKeyEvent| {
        counters.events.fetch_add(1, Ordering::Relaxed);
        let is_foreign = !is_chord_key(&targets, &raw.name);
        let t_secs = start.elapsed().as_secs_f64();
        let event = match raw.kind {
            super::manager::RawKeyKind::Press => {
                if is_foreign {
                    // `insert` returns false when the key was already held,
                    // which is exactly the auto-repeat case.
                    if let Ok(mut held) = held_foreign.lock() {
                        if held.insert(raw.name.clone()) {
                            counters.foreign_keys.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                CaptureEvent::KeyDown {
                    t_secs,
                    name: raw.name.clone(),
                }
            }
            super::manager::RawKeyKind::Release => {
                if is_foreign {
                    if let Ok(mut held) = held_foreign.lock() {
                        held.remove(&raw.name);
                    }
                }
                CaptureEvent::KeyUp {
                    t_secs,
                    name: raw.name.clone(),
                }
            }
        };
        let _ = tx.send(event);
    }
}

/// Non-feature build: the tap is never invoked (install returns Unsupported
/// before threads spawn), so return a zero-cost noop. Kept here so
/// `run_capture` compiles under both feature configurations.
///
/// Signature MUST match the feature-gated version — the caller in
/// `run_capture` passes `key_names.clone()` as the fourth argument
/// unconditionally, so this stub takes (and ignores) `_targets` too. Skipping
/// it broke the stock (`--no-default-features` / no `rust-hotkeys`) build with
/// an E0061 argument-count error while the feature build stayed green.
#[cfg(not(feature = "rust-hotkeys"))]
#[allow(clippy::unused_unit)]
fn build_raw_tap(
    _counters: Arc<Counters>,
    _tx: Sender<CaptureEvent>,
    _start: Instant,
    _targets: Vec<String>,
) -> impl Send + Sync + 'static {
    // `()` implements Send + Sync + 'static and satisfies the stock-build
    // `install_hotkey_with_raw_tap` bound. It is never invoked — the stock
    // install returns Unsupported before any listener thread spawns.
    ()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
    fn configure_confirmation_skips_capture_residue_before_yes() {
        let mut input = Cursor::new("\n\u{1b}[12~\nyes\n");
        let mut output = Vec::new();

        assert!(read_save_confirmation(&mut input, &mut output).unwrap());
        assert!(String::from_utf8(output).unwrap().contains("please answer"));
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

    #[test]
    fn captured_chord_preserves_modifier_side() {
        let mut capture = CapturedChord::default();
        capture.observe(&CaptureEvent::KeyDown {
            t_secs: 0.0,
            name: "ctrl_l".to_owned(),
        });
        capture.observe(&CaptureEvent::KeyDown {
            t_secs: 0.1,
            name: "f9".to_owned(),
        });
        capture.observe(&CaptureEvent::KeyUp {
            t_secs: 0.2,
            name: "f9".to_owned(),
        });
        assert_eq!(
            capture.observe(&CaptureEvent::KeyUp {
                t_secs: 0.3,
                name: "ctrl_l".to_owned(),
            }),
            Some("ctrl_l+f9".to_owned())
        );
    }

    #[test]
    fn captured_chord_rejects_new_keys_after_a_partial_release() {
        let mut capture = CapturedChord::default();
        for (name, pressed, t_secs) in [
            ("ctrl_l", true, 0.0),
            ("f9", true, 0.1),
            ("ctrl_l", false, 0.2),
            ("f10", true, 0.3),
            ("f9", false, 0.4),
            ("f10", false, 0.5),
        ] {
            assert_eq!(
                capture.observe(&if pressed {
                    CaptureEvent::KeyDown {
                        t_secs,
                        name: name.to_owned(),
                    }
                } else {
                    CaptureEvent::KeyUp {
                        t_secs,
                        name: name.to_owned(),
                    }
                }),
                None
            );
        }
    }

    #[test]
    fn captured_chord_accepts_only_installable_names() {
        assert_eq!(capture_key_name("ctrl_r"), Some("ctrl_r".to_owned()));
        assert_eq!(capture_key_name("f12"), Some("f12".to_owned()));
        assert_eq!(capture_key_name("backspace"), None);
        assert_eq!(capture_key_name("f13"), None);
    }

    #[test]
    fn unsupported_member_cancels_the_entire_candidate() {
        let mut capture = CapturedChord::default();
        capture.observe(&CaptureEvent::KeyDown {
            t_secs: 0.0,
            name: "ctrl_l".to_owned(),
        });
        capture.observe(&CaptureEvent::KeyDown {
            t_secs: 0.1,
            name: "a".to_owned(),
        });
        capture.observe(&CaptureEvent::KeyUp {
            t_secs: 0.2,
            name: "a".to_owned(),
        });
        assert_eq!(
            capture.observe(&CaptureEvent::KeyUp {
                t_secs: 0.3,
                name: "ctrl_l".to_owned(),
            }),
            None
        );
        capture.observe(&CaptureEvent::KeyDown {
            t_secs: 0.4,
            name: "ctrl_l".to_owned(),
        });
        capture.observe(&CaptureEvent::KeyDown {
            t_secs: 0.5,
            name: "f9".to_owned(),
        });
        capture.observe(&CaptureEvent::KeyUp {
            t_secs: 0.6,
            name: "f9".to_owned(),
        });
        assert_eq!(
            capture.observe(&CaptureEvent::KeyUp {
                t_secs: 0.7,
                name: "ctrl_l".to_owned(),
            }),
            Some("ctrl_l+f9".to_owned())
        );
    }

    // -----------------------------------------------------------------------
    // resolve_chord_key_names — `--chord` override precedence
    // -----------------------------------------------------------------------
    mod chord_override {
        use super::super::resolve_chord_key_names;
        use std::io::Write;

        /// Materialise a `settings.json` on disk with the given `key`
        /// value. Returned handle keeps the tempdir alive for the caller's
        /// scope. Kept private because every test in this module needs it.
        fn config_with_key(key: &str) -> (tempfile::TempDir, std::path::PathBuf) {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("settings.json");
            let contents = format!(r#"{{"key":"{key}","provider":"groq","toggle_mode":false}}"#);
            let mut f = std::fs::File::create(&path).expect("create");
            f.write_all(contents.as_bytes()).expect("write");
            (dir, path)
        }

        #[test]
        fn override_reaches_coordinator_bypassing_config() {
            // Precondition: config on disk points to a DIFFERENT chord.
            // With the override supplied, the coordinator must see the
            // override, not the config's chord.
            let (_dir, cfg_path) = config_with_key("ctrl_l+shift_l");
            let names = resolve_chord_key_names(Some("ctrl_l+alt_l+f9"), Some(cfg_path.as_path()))
                .expect("override + config resolves");
            assert_eq!(
                names,
                vec!["ctrl_l".to_owned(), "alt_l".to_owned(), "f9".to_owned(),],
                "the override, not the config's chord, must reach the coordinator",
            );
        }

        #[test]
        fn override_without_config_short_circuits_config_read() {
            // The whole point of `--chord`: verifying a chord without
            // touching the user's settings. A missing config path must
            // NOT surface as an error when the override is provided.
            let names = resolve_chord_key_names(Some("shift_r+f9"), None)
                .expect("override alone resolves without touching config");
            assert_eq!(names, vec!["shift_r".to_owned(), "f9".to_owned()]);
        }

        #[test]
        fn override_wins_even_when_config_has_empty_key() {
            // An empty `settings.key` is a hard error in the fallback
            // config path. The override MUST short-circuit that: someone
            // whose config has drifted to empty needs `--chord` to still
            // work so they can test candidate chords before saving one.
            let (_dir, cfg_path) = config_with_key("");
            let names = resolve_chord_key_names(Some("ctrl_l"), Some(cfg_path.as_path()))
                .expect("override wins over empty config key");
            assert_eq!(names, vec!["ctrl_l".to_owned()]);
        }

        #[test]
        fn empty_override_names_the_flag_in_the_error_message() {
            // The two error paths (empty --chord vs empty config key)
            // must surface distinctly so the operator knows WHICH input
            // was empty.
            let err = resolve_chord_key_names(Some("   "), None)
                .expect_err("whitespace override is rejected")
                .to_string();
            assert!(err.contains("--chord"), "error must name the flag: {err}");
            assert!(
                err.contains("+"),
                "error should hint at the `+`-separated format: {err}",
            );
        }

        #[test]
        fn config_error_wording_names_settings_key_when_override_is_none() {
            // Complement of the previous test: when there is no override,
            // the error must talk about `settings.key`, not `--chord`.
            let (_dir, cfg_path) = config_with_key("");
            let err = resolve_chord_key_names(None, Some(cfg_path.as_path()))
                .expect_err("empty config key without override is rejected")
                .to_string();
            assert!(
                err.contains("settings.key"),
                "error must name the config field, not the flag: {err}",
            );
            assert!(
                !err.contains("--chord"),
                "config-side error must not mention the CLI flag: {err}",
            );
        }
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
    fn validate_driver_flag_accepts_register_aliases() {
        // Keep the documented driver aliases parseable in both featureful
        // and stock builds; installation reports missing support later.
        assert!(validate_driver_flag("register").is_ok());
        assert!(validate_driver_flag("win_registerhotkey").is_ok());
        assert!(validate_driver_flag("wm_hotkey").is_ok());
        // Case + whitespace folding mirrors the canonical names.
        assert!(validate_driver_flag(" REGISTER ").is_ok());
        assert!(validate_driver_flag("Win_RegisterHotKey").is_ok());
        assert!(validate_driver_flag("WM_HOTKEY").is_ok());
    }

    #[test]
    fn configure_rejects_json_output() {
        let err = validate_configure_args(true, true, false, "auto", None)
            .expect_err("interactive capture must reject JSON output")
            .to_string();
        assert!(err.contains("--configure"));
        assert!(err.contains("--json"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn configure_rejects_register_driver() {
        let err = validate_configure_args(true, false, false, "register", None)
            .expect_err("register driver cannot expose raw key events")
            .to_string();
        assert!(err.contains("raw key-event listener"));
        assert!(err.contains("rdev"));
    }

    #[test]
    fn configure_rejects_chord_override() {
        let err = validate_configure_args(true, false, false, "auto", Some("ctrl+f9"))
            .expect_err("configure must not silently ignore --chord")
            .to_string();
        assert!(err.contains("--configure"));
        assert!(err.contains("--chord"));
    }

    #[test]
    fn configure_rejects_exit_on_chord() {
        let err = validate_configure_args(true, false, true, "auto", None)
            .expect_err("release-based capture cannot exit on the first press")
            .to_string();
        assert!(err.contains("--configure"));
        assert!(err.contains("--exit-on-chord"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn configure_allows_register_alias_for_platform_fallback() {
        assert!(validate_configure_args(true, false, false, "register", None).is_ok());
    }

    // -----------------------------------------------------------------------
    // counts_as_chord — the producer-side of the Chords: counter
    // -----------------------------------------------------------------------

    fn rec_id(n: u8) -> super::super::coordinator::RecordingId {
        super::super::coordinator::RecordingId::from(n)
    }

    #[test]
    fn counts_as_chord_true_only_for_start_recording() {
        assert!(counts_as_chord(&CoordinatorAction::StartRecording(rec_id(
            1
        ))));
        assert!(!counts_as_chord(&CoordinatorAction::StopAndTranscribe(
            rec_id(1)
        )));
        assert!(!counts_as_chord(&CoordinatorAction::CancelRecording(
            rec_id(1)
        )));
    }

    #[test]
    fn one_full_chord_cycle_increments_counter_once() {
        // Simulate what the action sink actually does: increment on every
        // action for which `counts_as_chord` is true. Two full chord cycles
        // (press → release, press → cancel) must report chords = 2, not 4.
        let counters = Counters::default();
        let cycle = [
            CoordinatorAction::StartRecording(rec_id(1)),
            CoordinatorAction::StopAndTranscribe(rec_id(1)),
            CoordinatorAction::StartRecording(rec_id(2)),
            CoordinatorAction::CancelRecording(rec_id(2)),
        ];
        for action in &cycle {
            if counts_as_chord(action) {
                counters.chords.fetch_add(1, Ordering::Relaxed);
            }
        }
        assert_eq!(counters.chords.load(Ordering::Relaxed), 2);
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

    // -----------------------------------------------------------------------
    // Counter plumbing (build_raw_tap + action sink)
    //
    // The pre-existing tests here only exercised the FORMATTERS with
    // hand-supplied numbers, so `foreign_keys` could be -- and was -- never
    // incremented anywhere while every test stayed green. These drive the
    // producing side instead.
    // -----------------------------------------------------------------------
    #[cfg(feature = "rust-hotkeys")]
    mod counters {
        use super::super::*;
        use crate::hotkey::manager::{RawKeyEvent, RawKeyKind, RawTap};
        use std::sync::mpsc;
        use std::time::Instant;

        fn ev(name: &str, kind: RawKeyKind) -> RawKeyEvent {
            RawKeyEvent {
                name: name.to_owned(),
                kind,
                at: Instant::now(),
            }
        }

        fn tap_with(targets: &[&str]) -> (Arc<Counters>, impl RawTap) {
            let counters = Arc::new(Counters::default());
            let (tx, rx) = mpsc::channel();
            // Keep the receiver alive for the tap's lifetime; the events
            // themselves are covered by the formatter tests.
            std::mem::forget(rx);
            let tap = build_raw_tap(
                Arc::clone(&counters),
                tx,
                Instant::now(),
                targets.iter().map(|s| (*s).to_owned()).collect(),
            );
            (counters, tap)
        }

        #[test]
        fn chord_keys_are_not_counted_as_foreign() {
            let (counters, tap) = tap_with(&["ctrl_l"]);
            tap.tap(&ev("ctrl_l", RawKeyKind::Press));
            tap.tap(&ev("ctrl_l", RawKeyKind::Release));
            assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 0);
            assert_eq!(counters.events.load(Ordering::Relaxed), 2);
        }

        #[test]
        fn foreign_press_is_counted() {
            let (counters, tap) = tap_with(&["ctrl_l"]);
            tap.tap(&ev("shift_l", RawKeyKind::Press));
            assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 1);
        }

        #[test]
        fn auto_repeat_counts_one_physical_press() {
            // A held key emits a stream of key-downs with no interleaved
            // key-up. The maintainer's real capture logged ~20 for one held
            // Shift; counting events would report 20 foreign keys.
            let (counters, tap) = tap_with(&["ctrl_l"]);
            for _ in 0..20 {
                tap.tap(&ev("shift_l", RawKeyKind::Press));
            }
            assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 1);
            // Released and pressed again -- that IS a second press.
            tap.tap(&ev("shift_l", RawKeyKind::Release));
            tap.tap(&ev("shift_l", RawKeyKind::Press));
            assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 2);
        }

        #[test]
        fn distinct_foreign_keys_count_separately() {
            let (counters, tap) = tap_with(&["ctrl_l"]);
            tap.tap(&ev("shift_l", RawKeyKind::Press));
            tap.tap(&ev("alt_l", RawKeyKind::Press));
            assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 2);
        }

        #[test]
        fn generic_modifier_target_matches_concrete_side() {
            // A `ctrl` binding must treat `ctrl_l` as part of the chord, the
            // same way the tracker's `is_target` does. Re-deriving this with
            // string equality would count it as foreign.
            let (counters, tap) = tap_with(&["ctrl"]);
            tap.tap(&ev("ctrl_l", RawKeyKind::Press));
            tap.tap(&ev("ctrl_r", RawKeyKind::Press));
            assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 0);
        }
    }

    // -----------------------------------------------------------------------
    // Coordinator loop-closing (the "deaf after the first chord" regression)
    // -----------------------------------------------------------------------
    #[cfg(feature = "rust-hotkeys")]
    mod processing_stage {
        use super::super::*;
        use crate::hotkey::coordinator::{self, CoordinatorEvent, Mode, Options};
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        /// Drive a real coordinator through TWO complete press -> release
        /// cycles, wiring the sink exactly the way `run_capture` does.
        ///
        /// Before the fix this yielded a single StartRecording: the first
        /// release parked the coordinator in `Stage::Processing` and nothing
        /// ever completed it, so every later press was swallowed. Observed on
        /// a real Wayland box as 14 keypresses producing no chord at all.
        ///
        /// The second cycle is sent only AFTER the first stop has been
        /// observed, mirroring reality -- a human cannot press again before
        /// the release they just made has been processed. Queueing all four
        /// events up front instead would land the second press inside
        /// `Processing`, where the coordinator legitimately defers it, and
        /// the test would be measuring event ordering rather than the missing
        /// feedback.
        fn start_recordings_over_two_cycles(complete_stage: bool) -> usize {
            let (action_tx, action_rx) = mpsc::channel();
            let slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
            let sink_slot = Arc::clone(&slot);
            let sink = move |action: CoordinatorAction| {
                if let CoordinatorAction::StopAndTranscribe(id) = action {
                    if complete_stage {
                        complete_processing_stage(sink_slot.get(), id);
                    }
                }
                let _ = action_tx.send(action);
            };
            // Inject a clock that jumps well past PRESS_DEBOUNCE (30 ms)
            // between events. Without it the second press is debounced away
            // and the test would "pass" for entirely the wrong reason -- it
            // would measure the debounce, not the Processing stage.
            let base = Instant::now();
            let mut ticks = 0u32;
            let clock = move || {
                ticks += 1;
                base + Duration::from_millis(100 * u64::from(ticks))
            };
            let (handle, thread) = coordinator::spawn(
                Options {
                    mode: Mode::HoldToTalk,
                    // This suite exercises the SINK-side completion path
                    // explicitly; auto-complete would defeat the point.
                    auto_complete_processing: false,
                },
                sink,
                clock,
            );
            let _ = slot.set(handle.clone());

            let mut starts = 0usize;
            let recv = |rx: &mpsc::Receiver<CoordinatorAction>| {
                rx.recv_timeout(Duration::from_millis(500))
            };

            handle.send(CoordinatorEvent::Press);
            handle.send(CoordinatorEvent::Release);
            // Cycle 1: expect StartRecording then StopAndTranscribe.
            if matches!(recv(&action_rx), Ok(CoordinatorAction::StartRecording(_))) {
                starts += 1;
            }
            let _ = recv(&action_rx);
            // Let the ProcessingFinished the sink just queued be consumed
            // before the next press -- this is the human gap.
            std::thread::sleep(Duration::from_millis(50));

            handle.send(CoordinatorEvent::Press);
            handle.send(CoordinatorEvent::Release);
            if matches!(recv(&action_rx), Ok(CoordinatorAction::StartRecording(_))) {
                starts += 1;
            }

            handle.shutdown();
            drop(thread);
            starts
        }

        #[test]
        fn second_chord_fires_when_the_processing_stage_is_completed() {
            assert_eq!(
                start_recordings_over_two_cycles(true),
                2,
                "both chords must fire once the sink completes the Processing stage"
            );
        }

        #[test]
        fn without_completion_the_coordinator_goes_deaf_after_one_chord() {
            // Pins the mechanism, so a future change that drops the
            // ProcessingFinished feedback fails loudly here instead of
            // silently making the diagnostic lie again.
            assert_eq!(
                start_recordings_over_two_cycles(false),
                1,
                "without ProcessingFinished the coordinator stays in Processing"
            );
        }

        /// Queue two press/release pairs without allowing the sink to run
        /// between them. Inline completion should leave the second pair
        /// ready to start instead of leaving the coordinator in Processing.
        fn starts_when_second_pair_is_queued_up_front(auto_complete: bool) -> usize {
            let (action_tx, action_rx) = mpsc::channel();
            let sink = move |action: CoordinatorAction| {
                let _ = action_tx.send(action);
            };
            let base = Instant::now();
            let mut ticks = 0u32;
            let clock = move || {
                ticks += 1;
                base + Duration::from_millis(100 * u64::from(ticks))
            };
            let (handle, thread) = coordinator::spawn(
                Options {
                    mode: Mode::HoldToTalk,
                    auto_complete_processing: auto_complete,
                },
                sink,
                clock,
            );

            // Queue both cycles up front so the completion ordering is tested.
            handle.send(CoordinatorEvent::Press);
            handle.send(CoordinatorEvent::Release);
            handle.send(CoordinatorEvent::Press);
            handle.send(CoordinatorEvent::Release);

            let mut starts = 0usize;
            while let Ok(action) = action_rx.recv_timeout(Duration::from_millis(500)) {
                if matches!(action, CoordinatorAction::StartRecording(_)) {
                    starts += 1;
                }
                if starts == 2 {
                    break;
                }
            }

            handle.shutdown();
            drop(thread);
            starts
        }

        #[test]
        fn auto_complete_lets_the_second_of_two_queued_pairs_fire() {
            assert_eq!(
                starts_when_second_pair_is_queued_up_front(true),
                2,
                "with auto_complete_processing the coordinator must reach \
                 Idle before the queued second Press is consumed",
            );
        }

        #[test]
        fn without_auto_complete_the_second_queued_pair_is_swallowed() {
            // Without inline completion, the coordinator remains in its
            // processing stage after the first release.
            assert_eq!(
                starts_when_second_pair_is_queued_up_front(false),
                1,
                "the second pair cannot start while processing remains pending",
            );
        }
    }
}
