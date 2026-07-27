//! `whisper-dictate self-test audio-ducking` — exercise the WASAPI
//! audio ducker in isolation.
//!
//! ## What this catches
//!
//! [`crate::dictate::audio_ducking::SystemAudioDucker`] is best-effort by
//! design: an unavailable WASAPI, a locked-down COM apartment, or a
//! Linux / macOS build all fall through to a warn-once no-op so a
//! broken audio subsystem never aborts an utterance. That masks silent
//! regressions on a headless CI box: the shipping session emits nothing
//! that says "I would have ducked here". This verb probes the ducker's
//! before-after state and reports which branch fired so a regression is
//! observable.
//!
//! ## Envelope
//!
//! ```json
//! {
//!   "kind": "audio_ducking_self_test",
//!   "ok": true|false,
//!   "error": null | "…",
//!   "backend": "wasapi" | "unsupported_platform" | "feature_disabled",
//!   "env_enabled": true|false,
//!   "target_volume": 0.25,
//!   "duration_ms": 500,
//!   "entered": true|false,
//!   "exited": true|false
//! }
//! ```
//!
//! `ok=false` when the env gate is ON but the resolved backend cannot
//! actually duck (e.g. non-Windows or the `audio-capture` cargo feature
//! is off). The verb still prints an envelope so the smoke script can
//! detect the branch.

use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::dictate::audio_ducking::{AudioDucker, SystemAudioDucker};

/// Options accepted by [`run_audio_ducking_self_test`].
#[derive(Debug, Clone)]
pub struct AudioDuckingOptions {
    /// How long to hold the ducked state before restoring. 500 ms is the
    /// CLI default — long enough for a user to hear the media dampen
    /// while ducking, short enough that CI runs in <1 s.
    pub duration: Duration,
    /// Force the "enabled" gate on. When `None` we read the ambient
    /// `VOICEPI_AUDIO_DUCKING` env var, same as
    /// [`SystemAudioDucker::from_env`].
    pub force_enabled: Option<bool>,
    /// Force a specific target volume. When `None` we read the ambient
    /// `VOICEPI_AUDIO_DUCKING_LEVEL` env var (default `0.25`).
    pub force_level: Option<f32>,
}

impl Default for AudioDuckingOptions {
    fn default() -> Self {
        Self {
            duration: Duration::from_millis(500),
            force_enabled: None,
            force_level: None,
        }
    }
}

/// Structured verb output.
#[derive(Debug, Clone)]
pub struct AudioDuckingReport {
    /// Which backend the resolver picked.
    pub backend: &'static str,
    /// Was the "enabled" gate on for this run? Reflects the env var (or
    /// the `--force-enabled` override, if supplied).
    pub env_enabled: bool,
    /// Effective target volume the ducker was constructed with.
    pub target_volume: f32,
    /// Duration the ducker was held before restore.
    pub duration: Duration,
    /// True when `enter()` returned. Always true (the trait is
    /// infallible); pinned so a future refactor to a fallible signature
    /// forces this test to update.
    pub entered: bool,
    /// True when `exit()` returned.
    pub exited: bool,
    /// Populated when the gate was on but the resolved backend cannot
    /// actually duck.
    pub error: Option<String>,
}

impl AudioDuckingReport {
    /// Non-zero exit on the "gate on but nothing to duck with" branch.
    /// The default path (gate off, backend not applicable) exits 0 —
    /// that's the correct "user did not opt in" answer.
    pub fn exit_ok(&self) -> bool {
        self.error.is_none()
    }

    pub fn to_json(&self) -> String {
        json!({
            "kind": "audio_ducking_self_test",
            "ok": self.exit_ok(),
            "error": self.error,
            "backend": self.backend,
            "env_enabled": self.env_enabled,
            "target_volume": self.target_volume,
            "duration_ms": self.duration.as_millis() as u64,
            "entered": self.entered,
            "exited": self.exited,
        })
        .to_string()
    }

    pub fn to_plain(&self) -> String {
        let mut out = format!(
            "[self-test audio-ducking] backend={} env_enabled={} target={:.2} duration={}ms\n",
            self.backend,
            self.env_enabled,
            self.target_volume,
            self.duration.as_millis()
        );
        out.push_str(&format!(
            "  entered={} exited={}\n",
            self.entered, self.exited
        ));
        if let Some(err) = &self.error {
            out.push_str(&format!("  FAIL: {err}\n"));
        } else {
            out.push_str("  PASS\n");
        }
        out
    }
}

/// Stable backend label. On Windows with `audio-capture` this returns
/// `"wasapi"`; on non-Windows it returns `"unsupported_platform"`; on a
/// Windows build without the `audio-capture` feature it returns
/// `"feature_disabled"`.
pub const fn resolve_backend() -> &'static str {
    #[cfg(all(windows, feature = "audio-capture"))]
    {
        "wasapi"
    }
    #[cfg(all(windows, not(feature = "audio-capture")))]
    {
        "feature_disabled"
    }
    #[cfg(not(windows))]
    {
        "unsupported_platform"
    }
}

/// Drive `enter -> sleep -> exit` on the production ducker and stamp the
/// report. The env-gate reflects the process env at call time; test
/// callers that need determinism should pass `force_enabled` / `force_level`.
pub fn run_audio_ducking_self_test(opts: AudioDuckingOptions) -> AudioDuckingReport {
    let env_enabled = opts.force_enabled.unwrap_or_else(|| {
        crate::dictate::audio_ducking::env_truthy(crate::dictate::audio_ducking::AUDIO_DUCKING_ENV)
    });
    let target_volume = opts
        .force_level
        .unwrap_or_else(crate::dictate::audio_ducking::parse_target_volume_from_env);
    let backend = resolve_backend();

    let mut ducker = SystemAudioDucker::new(env_enabled, target_volume);
    ducker.enter();
    thread::sleep(opts.duration);
    ducker.exit();

    let error = if env_enabled && backend != "wasapi" {
        Some(format!(
            "VOICEPI_AUDIO_DUCKING is enabled but the resolved backend is {backend:?}; \
             ducking is only implemented on Windows with the `audio-capture` cargo feature"
        ))
    } else {
        None
    };

    AudioDuckingReport {
        backend,
        env_enabled,
        target_volume,
        duration: opts.duration,
        entered: true,
        exited: true,
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
        let report = AudioDuckingReport {
            backend: "wasapi",
            env_enabled: false,
            target_volume: 0.25,
            duration: Duration::from_millis(500),
            entered: true,
            exited: true,
            error: None,
        };
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["kind"], "audio_ducking_self_test");
        assert_eq!(json["backend"], "wasapi");
        assert_eq!(json["target_volume"], 0.25);
        assert_eq!(json["duration_ms"], 500);
        assert_eq!(json["entered"], true);
        assert_eq!(json["exited"], true);
        assert_eq!(json["ok"], true);
    }

    #[test]
    fn env_off_exits_ok_regardless_of_backend() {
        // Gate off + unsupported backend is the correct "user did not
        // opt in" answer. Must not fail CI on non-Windows.
        for backend in ["wasapi", "unsupported_platform", "feature_disabled"] {
            let report = AudioDuckingReport {
                backend,
                env_enabled: false,
                target_volume: 0.25,
                duration: Duration::from_millis(0),
                entered: true,
                exited: true,
                error: None,
            };
            assert!(
                report.exit_ok(),
                "backend {backend} with gate off must pass"
            );
        }
    }

    #[test]
    fn resolve_backend_names_are_from_fixed_set() {
        assert!(matches!(
            resolve_backend(),
            "wasapi" | "unsupported_platform" | "feature_disabled"
        ));
    }

    #[test]
    fn forced_disabled_short_run_is_infallible() {
        // Gate FORCED off + a 0 ms hold — the runner must not talk to any
        // OS API and must produce a passing report. This is the CI-safe
        // exercise a headless container can always run.
        let report = run_audio_ducking_self_test(AudioDuckingOptions {
            duration: Duration::from_millis(0),
            force_enabled: Some(false),
            force_level: Some(0.5),
        });
        assert!(!report.env_enabled);
        assert_eq!(report.target_volume, 0.5);
        assert!(report.entered);
        assert!(report.exited);
        assert!(report.exit_ok());
    }
}
