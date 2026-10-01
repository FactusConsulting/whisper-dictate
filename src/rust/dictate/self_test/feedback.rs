//! `whisper-dictate self-test feedback` — exercise the Rust-engine PTT
//! start / stop / processing-complete audible cues in isolation.
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
//!   "done_played": true|false,
//!   "delay_ms": 100
//! }
//! ```
//!
//! `ok=false` means an enabled cue had no resolvable backend — that's the
//! "silently muted" regression this verb exists to catch.

use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::dictate::feedback::{cue_enabled_by_env, CueKind, CueSink, SystemCueSink};

/// Options accepted by [`run_feedback_self_test`]. Kept as a struct so the
/// CLI wrapper can grow flags (custom delay, override player, …) without
/// changing the runner's arity.
#[derive(Debug, Clone)]
pub struct FeedbackOptions {
    /// Sleep between selected cues so a listener can hear them. The
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
    /// True when the selected start cue was sent to an available backend.
    pub start_played: bool,
    /// Same for `Stop`.
    pub stop_played: bool,
    /// Same for the optional processing-complete cue.
    pub done_played: bool,
    /// Delay honoured between selected cue calls.
    pub delay: Duration,
    /// Populated with an actionable message when [`Self::exit_ok`] is
    /// false. `None` on the happy path.
    pub error: Option<String>,
}

impl FeedbackReport {
    /// Non-zero exit is warranted when a cue is selected but no backend
    /// exists. An intentionally disabled gate or all-disabled events pass.
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
            "done_played": self.done_played,
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
            "  start_played={} stop_played={} done_played={}\n",
            self.start_played, self.stop_played, self.done_played
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

/// Drive each enabled cue through the production [`SystemCueSink`] and stamp
/// the report. Never panics — [`CueSink::play`] is infallible by contract.
pub fn run_feedback_self_test(opts: FeedbackOptions) -> FeedbackReport {
    run_feedback_self_test_with_sink(opts, resolve_backend(), &SystemCueSink)
}

fn missing_selected_asset(backend: &str, selected: &[CueKind]) -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        if matches!(backend, "paplay" | "pw-play") {
            return selected
                .iter()
                .map(|kind| crate::dictate::feedback::freedesktop_cue_file(*kind))
                .find(|path| !std::path::Path::new(path).is_file());
        }
    }
    let _ = (backend, selected);
    None
}

fn run_feedback_self_test_with_sink(
    opts: FeedbackOptions,
    backend: &'static str,
    sink: &dyn CueSink,
) -> FeedbackReport {
    let env_enabled = crate::dictate::feedback::sounds_enabled();
    let selected: Vec<_> = [CueKind::Start, CueKind::Stop, CueKind::Done]
        .into_iter()
        .filter(|kind| cue_enabled_by_env(*kind))
        .collect();
    let missing_asset = missing_selected_asset(backend, &selected);
    let playable = backend != "noop" && missing_asset.is_none();
    if playable {
        for (index, kind) in selected.iter().enumerate() {
            if index > 0 {
                thread::sleep(opts.delay);
            }
            sink.play(*kind);
        }
    }
    let start_played = playable && selected.contains(&CueKind::Start);
    let stop_played = playable && selected.contains(&CueKind::Stop);
    let done_played = playable && selected.contains(&CueKind::Done);
    let error = if env_enabled && !selected.is_empty() && backend == "noop" {
        // Gate on but nothing available — the shipping session would be
        // silently muted. Surface this so CI (and a user smoke run)
        // trips.
        Some(
            "VOICEPI_FEEDBACK_SOUNDS is enabled but no playback backend is available; \
             on Linux install paplay (pulseaudio-utils) or pw-play (pipewire-utils)"
                .to_owned(),
        )
    } else {
        missing_asset.map(|path| format!("selected feedback cue asset is missing: {path}"))
    };
    FeedbackReport {
        env_enabled,
        backend,
        start_played,
        stop_played,
        done_played,
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
    use std::sync::Mutex;

    struct RecordingSink(Mutex<Vec<CueKind>>);

    impl CueSink for RecordingSink {
        fn play(&self, kind: CueKind) {
            self.0.lock().unwrap().push(kind);
        }
    }

    #[test]
    fn self_test_exercises_done_when_it_is_the_only_enabled_event() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let vars = [
            ("VOICEPI_FEEDBACK_SOUNDS", "1"),
            ("VOICEPI_FEEDBACK_START", "0"),
            ("VOICEPI_FEEDBACK_STOP", "0"),
            ("VOICEPI_FEEDBACK_DONE", "1"),
        ];
        let prior: Vec<_> = vars
            .iter()
            .map(|(name, _)| std::env::var(name).ok())
            .collect();
        for (name, value) in vars {
            std::env::set_var(name, value);
        }

        let sink = RecordingSink(Mutex::new(Vec::new()));
        let report = run_feedback_self_test_with_sink(
            FeedbackOptions {
                delay: Duration::ZERO,
            },
            "kernel32_beep",
            &sink,
        );
        assert_eq!(*sink.0.lock().unwrap(), [CueKind::Done]);
        assert!(!report.start_played);
        assert!(!report.stop_played);
        assert!(report.done_played);
        assert!(report.exit_ok());

        sink.0.lock().unwrap().clear();
        let no_backend = run_feedback_self_test_with_sink(
            FeedbackOptions {
                delay: Duration::ZERO,
            },
            "noop",
            &sink,
        );
        assert!(sink.0.lock().unwrap().is_empty());
        assert!(!no_backend.done_played);
        assert!(!no_backend.exit_ok());

        for ((name, _), value) in vars.into_iter().zip(prior) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn self_test_defaults_to_legacy_start_and_stop_sequence() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let names = [
            "VOICEPI_FEEDBACK_SOUNDS",
            "VOICEPI_FEEDBACK_START",
            "VOICEPI_FEEDBACK_STOP",
            "VOICEPI_FEEDBACK_DONE",
        ];
        let prior: Vec<_> = names.iter().map(|name| std::env::var(name).ok()).collect();
        std::env::set_var(names[0], "1");
        for name in &names[1..] {
            std::env::remove_var(name);
        }

        let sink = RecordingSink(Mutex::new(Vec::new()));
        let report = run_feedback_self_test_with_sink(
            FeedbackOptions {
                delay: Duration::ZERO,
            },
            "kernel32_beep",
            &sink,
        );
        assert_eq!(*sink.0.lock().unwrap(), [CueKind::Start, CueKind::Stop]);
        assert!(report.start_played);
        assert!(report.stop_played);
        assert!(!report.done_played);

        for (name, value) in names.into_iter().zip(prior) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn report_json_has_stable_keys() {
        let report = FeedbackReport {
            env_enabled: false,
            backend: "noop",
            start_played: false,
            stop_played: false,
            done_played: false,
            delay: Duration::from_millis(100),
            error: None,
        };
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["kind"], "feedback_self_test");
        assert_eq!(json["ok"], true);
        assert_eq!(json["env_enabled"], false);
        assert_eq!(json["backend"], "noop");
        assert_eq!(json["start_played"], false);
        assert_eq!(json["stop_played"], false);
        assert_eq!(json["done_played"], false);
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
            start_played: false,
            stop_played: false,
            done_played: false,
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
            start_played: false,
            stop_played: false,
            done_played: false,
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
            start_played: false,
            stop_played: false,
            done_played: false,
            delay: Duration::from_millis(0),
            error: None,
        };
        assert!(ok.to_plain().contains("PASS"));

        let bad = FeedbackReport {
            env_enabled: true,
            backend: "noop",
            start_played: false,
            stop_played: false,
            done_played: false,
            delay: Duration::from_millis(0),
            error: Some("broken".to_owned()),
        };
        assert!(bad.to_plain().contains("FAIL"));
    }
}
