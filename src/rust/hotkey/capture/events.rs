//! Capture protocol, chord recognition and stable output formatting.

use crate::config::{load_settings, load_settings_from_path};
use anyhow::{anyhow, Result};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

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
    ChordMatched {
        t_secs: f64,
        id: u64,
        focused: Option<bool>,
    },
    /// The tracker observed the chord release (falling edge).
    ChordReleased {
        t_secs: f64,
        id: u64,
        focused: Option<bool>,
    },
    /// The tracker cancelled the in-flight chord — either a foreign key
    /// joined the modifier(s) mid-recording (bare-modifier rule 2) or the
    /// coordinator was reset. Included so operators can tell "chord broke
    /// because of foreign key" apart from "chord broke because of release".
    ChordCanceled {
        t_secs: f64,
        id: u64,
        focused: Option<bool>,
    },
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
pub(super) struct CapturedChord {
    held: BTreeSet<String>,
    seen: BTreeSet<String>,
    invalid: bool,
    changed_after_release: bool,
}

impl CapturedChord {
    pub(super) fn observe(&mut self, event: &CaptureEvent) -> Option<String> {
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
            if self.held.contains(&name) {
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

pub(crate) fn format_captured_chord(keys: &BTreeSet<String>) -> String {
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
    pub(super) fn is_terminal(&self) -> bool {
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
        CaptureEvent::ChordMatched { t_secs, id, .. } => {
            format!("{OUTPUT_PREFIX} {t_secs:.3}s CHORD MATCHED (id={id})")
        }
        CaptureEvent::ChordReleased { t_secs, id, .. } => {
            format!("{OUTPUT_PREFIX} {t_secs:.3}s CHORD RELEASED (id={id})")
        }
        CaptureEvent::ChordCanceled { t_secs, id, .. } => {
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
        CaptureEvent::ChordMatched {
            t_secs,
            id,
            focused,
        } => json!({
            "t": round3(*t_secs),
            "kind": "chord_matched",
            "id": id,
            "focused": focused,
        }),
        CaptureEvent::ChordReleased {
            t_secs,
            id,
            focused,
        } => json!({
            "t": round3(*t_secs),
            "kind": "chord_released",
            "id": id,
            "focused": focused,
        }),
        CaptureEvent::ChordCanceled {
            t_secs,
            id,
            focused,
        } => json!({
            "t": round3(*t_secs),
            "kind": "chord_canceled",
            "id": id,
            "focused": focused,
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
/// Factored out of [`super::command::run_capture`] so precedence and coordinator input can be
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

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
