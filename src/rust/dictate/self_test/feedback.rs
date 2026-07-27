//! `whisper-dictate self-test feedback` — exercise the Rust-engine PTT
//! start / stop audible cues in isolation.
//!
//! ## What this catches
//!
//! [`crate::dictate::feedback`] is a small module but its failure modes
//! are all silent: `VOICEPI_FEEDBACK_SOUNDS` gated off, no `paplay` /
//! `pw-play` on `$PATH`, the freedesktop sound files missing, a broken
//! `kernel32!Beep` on a locked-down Windows install. Because the module
//! swallows every error by design (a broken audio subsystem must never
//! abort an utterance) a regression would ship silently.
//!
//! This verb probes the SAME resolution the shipping [`SystemCueSink`]
//! runs — env gate + platform selector + backend availability — WITHOUT
//! blocking on the ~80 ms beep thread the production path spawns
//! (spawning the thread is enough to prove the code path fires; the
//! kernel-level playback is not observable from a headless CI box).
//!
//! ## Envelope
//!
//! ```json
//! {
//!   "kind": "feedback_self_test",
//!   "ok": true|false,
//!   "error": null | "…",
//!   "env_enabled": true|false,
//!   "backend": "kernel32_beep" | "paplay" | "pw-play" | "noop",
//!   "start_played": true|false,
//!   "stop_played": true|false,
//!   "delay_ms": 100
//! }
//! ```
//!
//! `ok=false` means the env gate was ON but no backend was resolvable —
//! that's the "silently muted" regression this verb exists to catch.

use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::dictate::feedback::{CueKind, CueSink, SystemCueSink};

/// Options accepted by [`run_feedback_self_test`]. Kept as a struct so the
/// CLI wrapper can grow flags (custom delay, override player, …) without
/// changing the runner's arity.
#[derive(Debug, Clone)]
pub struct FeedbackOptions {
    /// Sleep between start and stop cues so a listener can hear both. The
    /// verb still passes with `0` — the check is code-path level, not
    /// audible.
    pub delay: Duration,
}

impl Default for FeedbackOptions {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(100),
        }
    }
}

/// Verb output. Rendered to JSON via [`Self::to_json`]; [`Self::exit_ok`]
/// tells the CLI whether to exit non-zero.
#[derive(Debug, Clone)]
pub struct FeedbackReport {
    /// Was `VOICEPI_FEEDBACK_SOUNDS` truthy at run time?
    pub env_enabled: bool,
    /// Which backend the resolver picked. `"noop"` on macOS / other
    /// platforms or when nothing is available.
    pub backend: &'static str,
    /// True when [`SystemCueSink::play(CueKind::Start)`] returned without
    /// panicking (the trait contract already guarantees infallibility;
    /// this field pins that the call *ran*).
    pub start_played: bool,
    /// Same for `Stop`.
    pub stop_played: bool,
    /// Delay honoured between the two calls.
    pub delay: Duration,
    /// Populated with an actionable message when [`Self::exit_ok`] is
    /// false. `None` on the happy path.
    pub error: Option<String>,
}

impl FeedbackReport {
    /// Non-zero exit is warranted when the env gate is ON but no backend
    /// could actually play a cue — that's the silent-mute regression we
    /// want CI to trip on. When the gate is OFF a `"noop"` backend is the
    /// correct answer, so exit 0.
    pub fn exit_ok(&self) -> bool {
        self.error.is_none()
    }

    /// Machine-readable envelope. Stable keys are the smoke-script
    /// contract.
    pub fn to_json(&self) -> String {
        json!({
            "kind": "feedback_self_test",
            "ok": self.exit_ok(),
            "error": self.error,
            "env_enabled": self.env_enabled,
            "backend": self.backend,
            "start_played": self.start_played,
            "stop_played": self.stop_played,
            "delay_ms": self.delay.as_millis() as u64,
        })
        .to_string()
    }

    /// Terse plain-text summary. The JSON form is the primary output; the
    /// plain form is a fallback for interactive use.
    pub fn to_plain(&self) -> String {
        let mut out = format!(
            "[self-test feedback] env_enabled={} backend={} delay={}ms\n",
            self.env_enabled,
            self.backend,
            self.delay.as_millis()
        );
        out.push_str(&format!(
            "  start_played={} stop_played={}\n",
            self.start_played, self.stop_played
        ));
        if let Some(err) = &self.error {
            out.push_str(&format!("  FAIL: {err}\n"));
        } else {
            out.push_str("  PASS\n");
        }
        out
    }
}

/// Resolve which backend the shipping [`SystemCueSink`] would use *right
/// now*. Kept as a free function so the CLI's report and any future
/// diagnostics both stamp the same string.
///
/// Returns one of `"kernel32_beep"`, `"paplay"`, `"pw-play"`, `"noop"`.
/// The Windows and Linux checks match the exact selector the module
/// itself uses (see [`crate::dictate::feedback`] module docs); on macOS /
/// other targets the module deliberately no-ops and this function
/// reports `"noop"`.
pub fn resolve_backend() -> &'static str {
    #[cfg(windows)]
    {
        "kernel32_beep"
    }
    #[cfg(target_os = "linux")]
    {
        // Mirror `feedback::LINUX_PLAYERS` order: paplay > pw-play. We
        // probe `$PATH` with `which`-style lookups without shelling out
        // so the verb runs on containers without `which` available.
        for player in ["paplay", "pw-play"] {
            if which_on_path(player) {
                return match player {
                    "paplay" => "paplay",
                    "pw-play" => "pw-play",
                    _ => "noop",
                };
            }
        }
        "noop"
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        "noop"
    }
}

/// Minimal `$PATH` search used by [`resolve_backend`] on Linux. Kept
/// module-local so the self-test doesn't pull in the `which` crate.
#[cfg(target_os = "linux")]
fn which_on_path(name: &str) -> bool {
    let Some(path_env) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_env) {
        if dir.join(name).is_file() {
            return true;
        }
    }
    false
}

/// Drive both cues through the production [`SystemCueSink`] and stamp
/// the report. Never panics — [`CueSink::play`] is infallible by
/// contract.
pub fn run_feedback_self_test(opts: FeedbackOptions) -> FeedbackReport {
    let env_enabled = crate::dictate::feedback::sounds_enabled();
    let backend = resolve_backend();
    let sink = SystemCueSink;
    sink.play(CueKind::Start);
    thread::sleep(opts.delay);
    sink.play(CueKind::Stop);
    let start_played = true;
    let stop_played = true;
    let error = if env_enabled && backend == "noop" {
        // Gate on but nothing available — the shipping session would be
        // silently muted. Surface this so CI (and a user smoke run)
        // trips.
        Some(
            "VOICEPI_FEEDBACK_SOUNDS is enabled but no playback backend is available; \
             on Linux install paplay (pulseaudio-utils) or pw-play (pipewire-utils)"
                .to_owned(),
        )
    } else {
        None
    };
    FeedbackReport {
        env_enabled,
        backend,
        start_played,
        stop_played,
        delay: opts.delay,
        error,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_json_has_stable_keys() {
        let report = FeedbackReport {
            env_enabled: false,
            backend: "noop",
            start_played: true,
            stop_played: true,
            delay: Duration::from_millis(100),
            error: None,
        };
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["kind"], "feedback_self_test");
        assert_eq!(json["ok"], true);
        assert_eq!(json["env_enabled"], false);
        assert_eq!(json["backend"], "noop");
        assert_eq!(json["start_played"], true);
        assert_eq!(json["stop_played"], true);
        assert_eq!(json["delay_ms"], 100);
        assert!(json["error"].is_null());
    }

    #[test]
    fn env_off_with_noop_backend_is_ok() {
        // Gate off + noop backend is the correct "cues disabled" answer.
        // The exit code must not fail on this — the smoke script would
        // otherwise trip on every headless CI leg.
        let report = FeedbackReport {
            env_enabled: false,
            backend: "noop",
            start_played: true,
            stop_played: true,
            delay: Duration::from_millis(0),
            error: None,
        };
        assert!(report.exit_ok());
    }

    #[test]
    fn env_on_but_noop_backend_is_a_fail() {
        // The whole point of the verb: env on but nothing to play with
        // means the user's cues would be silently muted in production.
        let report = FeedbackReport {
            env_enabled: true,
            backend: "noop",
            start_played: true,
            stop_played: true,
            delay: Duration::from_millis(0),
            error: Some("silently muted".to_owned()),
        };
        assert!(!report.exit_ok());
    }

    #[test]
    fn resolve_backend_names_are_from_fixed_set() {
        assert!(matches!(
            resolve_backend(),
            "kernel32_beep" | "paplay" | "pw-play" | "noop"
        ));
    }

    #[test]
    fn plain_report_marks_pass_or_fail() {
        let ok = FeedbackReport {
            env_enabled: false,
            backend: "noop",
            start_played: true,
            stop_played: true,
            delay: Duration::from_millis(0),
            error: None,
        };
        assert!(ok.to_plain().contains("PASS"));

        let bad = FeedbackReport {
            env_enabled: true,
            backend: "noop",
            start_played: true,
            stop_played: true,
            delay: Duration::from_millis(0),
            error: Some("broken".to_owned()),
        };
        assert!(bad.to_plain().contains("FAIL"));
    }
}
