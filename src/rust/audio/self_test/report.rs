//! Report shape for `self-test audio-capture`.
//!
//! Split from the runner so the JSON/plain contract can be exercised by
//! unit tests without opening a cpal stream — the smoke script and the
//! CI leg pin the field names here and a rename must trip a compile
//! error before it surprises the wire consumer.

use std::time::Duration;

use serde_json::json;

use crate::audio::pipewire::QuantumDecision;

/// Report shape emitted by [`super::runner::run_audio_capture_test`].
/// Fields are the machine-readable contract callers (the
/// wayland-user-smoke script, the Ubuntu 26.04 CI leg) pin against — a
/// rename here must be deliberate.
#[derive(Debug, Clone)]
pub struct AudioCaptureReport {
    /// Duration the caller asked for (echoed back so a truncated run is
    /// obvious).
    pub requested_duration: Duration,
    /// The device selector we resolved against cpal. Empty string means
    /// "system default input" — the caller can echo that back to the user
    /// without special-casing.
    pub device: String,
    /// PipeWire quantum decision on Linux (echoes back what
    /// [`crate::audio::pipewire::configure_pipewire_env`] did). On
    /// non-Linux the decision is a distinctive `UserOverride("")`
    /// marker so the JSON consumer can tell the branch didn't fire.
    pub quantum_decision: QuantumDecision,
    /// Number of `AudioChunk::Samples` messages the pump received. Zero
    /// means the stream opened but never delivered — the v1.20.6 crash
    /// class we care about most.
    pub frames_captured: usize,
    /// Total mono samples across every chunk (post-mix-to-mono). Zero when
    /// `frames_captured` is zero; otherwise a sanity check that chunks
    /// aren't empty.
    pub samples_captured: usize,
    /// Root-mean-square of the captured signal, in `[0.0, ~1.0]`. Zero
    /// when no samples arrived.
    pub rms: f32,
    /// Peak absolute sample value, in `[0.0, 1.0]`. Zero if no samples
    /// arrived.
    pub peak: f32,
    /// The native sample rate cpal negotiated with the device. Zero on
    /// device-open failure. Useful when the caller wants to correlate the
    /// RMS with a specific device configuration.
    pub sample_rate: u32,
    /// Human-readable error message on failure; empty on success.
    pub error: String,
    /// Whether the capture path succeeded end-to-end (device opened AND
    /// at least one chunk delivered).
    pub succeeded: bool,
}

impl AudioCaptureReport {
    /// True iff the capture path fully worked — a device was opened, and
    /// at least one chunk of samples arrived before the deadline. Silence
    /// is still a success at this layer (the operator can `--fail-on-silence`
    /// if they want to promote it).
    pub fn is_ok(&self) -> bool {
        self.succeeded
    }

    /// Render the report as a single JSON object. Keys are the stable
    /// contract the smoke script / CI leg pin against.
    pub fn to_json(&self) -> String {
        let quantum_num = self
            .quantum_decision
            .quantum()
            .map(|q| json!(q))
            .unwrap_or(json!(null));
        json!({
            "kind": "audio_capture_self_test",
            "device": self.device,
            "requested_duration_ms": self.requested_duration.as_millis() as u64,
            "frames_captured": self.frames_captured,
            "samples_captured": self.samples_captured,
            "rms": self.rms,
            "peak": self.peak,
            "sample_rate": self.sample_rate,
            "pipewire_quantum_branch": self.quantum_decision.as_str(),
            "pipewire_quantum": quantum_num,
            "succeeded": self.succeeded,
            "error": self.error,
        })
        .to_string()
    }

    /// Human-readable multi-line summary. Mirrors the shape of the
    /// injection-idempotency report so an operator eyeballing the smoke
    /// output gets the same feel across verbs.
    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        let device_label = if self.device.is_empty() {
            "<default>".to_owned()
        } else {
            self.device.clone()
        };
        out.push_str(&format!(
            "[self-test audio-capture] device={}  duration_ms={}  quantum={}\n",
            device_label,
            self.requested_duration.as_millis(),
            match self.quantum_decision.quantum() {
                Some(q) => format!("{q} ({branch})", branch = self.quantum_decision.as_str()),
                None => format!("? ({branch})", branch = self.quantum_decision.as_str()),
            },
        ));
        if self.succeeded {
            out.push_str(&format!(
                "  frames={}  samples={}  sample_rate={}  rms={:.4}  peak={:.4}  RESULT: PASS\n",
                self.frames_captured, self.samples_captured, self.sample_rate, self.rms, self.peak,
            ));
        } else {
            out.push_str(&format!("  RESULT: FAIL — {}\n", self.error));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_report() -> AudioCaptureReport {
        AudioCaptureReport {
            requested_duration: Duration::from_secs(1),
            device: String::new(),
            quantum_decision: QuantumDecision::ApplyDefault(2048),
            frames_captured: 4,
            samples_captured: 48_000,
            rms: 0.123,
            peak: 0.5,
            sample_rate: 48_000,
            error: String::new(),
            succeeded: true,
        }
    }

    #[test]
    fn report_json_has_stable_keys() {
        let json = ok_report().to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["kind"], "audio_capture_self_test");
        assert_eq!(parsed["device"], "");
        assert_eq!(parsed["requested_duration_ms"], 1000);
        assert_eq!(parsed["frames_captured"], 4);
        assert_eq!(parsed["samples_captured"], 48_000);
        assert_eq!(parsed["sample_rate"], 48_000);
        assert_eq!(parsed["pipewire_quantum_branch"], "default_applied");
        assert_eq!(parsed["pipewire_quantum"], 2048);
        assert_eq!(parsed["succeeded"], true);
        // rms + peak survive the JSON round-trip within f32 precision.
        let rms = parsed["rms"].as_f64().expect("rms is a number");
        assert!((rms - 0.123).abs() < 1e-4, "rms {rms}");
    }

    #[test]
    fn report_json_encodes_user_override_quantum_as_string_branch() {
        let r = AudioCaptureReport {
            requested_duration: Duration::from_millis(500),
            device: "mic".to_owned(),
            quantum_decision: QuantumDecision::UserOverride("512".to_owned()),
            frames_captured: 0,
            samples_captured: 0,
            rms: 0.0,
            peak: 0.0,
            sample_rate: 0,
            error: "no samples".to_owned(),
            succeeded: false,
        };
        let parsed: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(parsed["pipewire_quantum_branch"], "user_override");
        assert_eq!(parsed["pipewire_quantum"], 512);
        assert_eq!(parsed["succeeded"], false);
        assert_eq!(parsed["error"], "no samples");
    }

    #[test]
    fn report_json_encodes_non_numeric_user_override_as_null_quantum() {
        // A garbage override (say `PIPEWIRE_QUANTUM=broken`) surfaces as
        // `pipewire_quantum: null` in the JSON so a diagnostic can flag
        // it without our code silently dropping the user's setting.
        let r = AudioCaptureReport {
            requested_duration: Duration::from_millis(100),
            device: String::new(),
            quantum_decision: QuantumDecision::UserOverride("garbage".to_owned()),
            frames_captured: 0,
            samples_captured: 0,
            rms: 0.0,
            peak: 0.0,
            sample_rate: 0,
            error: String::new(),
            succeeded: false,
        };
        let parsed: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert!(parsed["pipewire_quantum"].is_null());
        assert_eq!(parsed["pipewire_quantum_branch"], "user_override");
    }

    #[test]
    fn report_to_plain_names_pass_or_fail() {
        assert!(ok_report().to_plain().contains("RESULT: PASS"));

        let fail = AudioCaptureReport {
            requested_duration: Duration::from_secs(1),
            device: "missing".to_owned(),
            quantum_decision: QuantumDecision::ApplyDefault(2048),
            frames_captured: 0,
            samples_captured: 0,
            rms: 0.0,
            peak: 0.0,
            sample_rate: 0,
            error: "input device not found".to_owned(),
            succeeded: false,
        };
        let plain = fail.to_plain();
        assert!(plain.contains("RESULT: FAIL"), "plain: {plain}");
        assert!(plain.contains("input device not found"), "plain: {plain}");
    }

    #[test]
    fn is_ok_matches_succeeded_flag() {
        let mut r = ok_report();
        assert!(r.is_ok());
        r.succeeded = false;
        assert!(!r.is_ok());
    }

    #[test]
    fn plain_output_labels_default_device_distinctly_from_named() {
        let default = ok_report();
        assert!(default.to_plain().contains("device=<default>"));

        let named = AudioCaptureReport {
            device: "Yeti Classic".to_owned(),
            ..ok_report()
        };
        assert!(named.to_plain().contains("device=Yeti Classic"));
    }
}
