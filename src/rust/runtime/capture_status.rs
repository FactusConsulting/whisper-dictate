//! Runtime status and metering for the push-to-talk microphone lifecycle.
//!
//! Every capture diagnostic flows through [`CaptureReporter`] so the UI gets
//! the same `[worker-event]` status contract as before #323:
//! `audio-fallback`, `audio-recovered`, and `error` with
//! `reason="device_unusable"`.
//! Live readings are sent separately as `audio` worker events.

#![cfg(feature = "audio-capture")]
// The production caller (`rust_session_audio`) needs the full backend set.
#![cfg_attr(not(native_session), allow(dead_code))]

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};

use crate::runtime::{RepaintNotifier, RuntimeEvent, WorkerEvent};

/// Prefix every capture log line carries so a user grepping a log can pin
/// the source.
pub(crate) const LOG_PREFIX: &str = "[rust-session-audio]";

/// Effective-device label published while the OS default input is used in
/// place of a configured microphone.
pub(crate) const SYSTEM_DEFAULT_LABEL: &str = "System default";

const METER_FLOOR_DBFS: f64 = -60.0;
const SILENCE_DBFS: f64 = -120.0;

/// A reading from one nonempty, unnormalized 16 kHz frame.
#[derive(Clone, Copy)]
pub(super) struct AudioMeterReading {
    raw_dbfs: f64,
    peak: f64,
}

impl AudioMeterReading {
    pub(super) fn measure(frame: &[f32]) -> Self {
        let mut sum_squares = 0.0;
        let mut peak = 0.0_f64;
        for &sample in frame {
            if sample.is_finite() {
                let sample = f64::from(sample);
                sum_squares += sample * sample;
                peak = peak.max(sample.abs());
            }
        }
        // 10 * log10(mean square) is 20 * log10(RMS), without the square root.
        let mean_square = sum_squares / frame.len() as f64;
        Self {
            raw_dbfs: (10.0 * mean_square.log10()).clamp(SILENCE_DBFS, 0.0),
            peak: peak.min(1.0),
        }
    }
}

/// Publishes capture statuses on the runtime channel and keeps the session's
/// effective-device label in sync without touching the session mutex.
pub(crate) struct CaptureReporter {
    tx: Mutex<Sender<RuntimeEvent>>,
    repaint_notifier: Option<RepaintNotifier>,
    effective_audio_device: Arc<RwLock<String>>,
}

impl CaptureReporter {
    pub(crate) fn new(
        tx: Sender<RuntimeEvent>,
        repaint_notifier: Option<RepaintNotifier>,
        effective_audio_device: Arc<RwLock<String>>,
    ) -> Self {
        Self {
            tx: Mutex::new(tx),
            repaint_notifier,
            effective_audio_device,
        }
    }

    pub(crate) fn set_effective_device(&self, device: &str) {
        *self
            .effective_audio_device
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = device.to_owned();
    }

    /// Plain log line on the runtime channel (shown in the UI debug log).
    pub(crate) fn stderr(&self, message: String) {
        self.send(RuntimeEvent::Stderr(message));
    }

    pub(crate) fn status(
        &self,
        state: &str,
        audio_device: Option<&str>,
        reason: Option<&str>,
        error: Option<&str>,
    ) {
        self.send(audio_status_event(state, audio_device, reason, error));
    }

    /// Queue while holding the capture slot lock, then notify after releasing it.
    /// `None` clears the meter when the size cap closes the microphone early.
    pub(super) fn enqueue_audio_meter(&self, reading: Option<AudioMeterReading>) {
        let active = reading.is_some();
        let state = if active { "recording" } else { "capture_lost" };
        let AudioMeterReading { raw_dbfs, peak } = reading.unwrap_or(AudioMeterReading {
            raw_dbfs: SILENCE_DBFS,
            peak: 0.0,
        });
        let level = ((raw_dbfs - METER_FLOOR_DBFS) / -METER_FLOOR_DBFS).clamp(0.0, 1.0);
        let mut payload = serde_json::json!({
            "event": "audio",
            "state": state,
            "level": level,
            "raw_dbfs": raw_dbfs,
            "peak": peak,
            "active": active,
        });
        {
            let device = self
                .effective_audio_device
                .read()
                .unwrap_or_else(|poison| poison.into_inner());
            if !device.is_empty() {
                payload["audio_device"] = serde_json::Value::from(device.as_str());
            }
        }
        self.enqueue(RuntimeEvent::Worker(WorkerEvent {
            event: "audio".to_owned(),
            state: Some(state.to_owned()),
            payload,
        }));
    }

    /// Clear the effective device and raise the persistent
    /// `device_unusable` banner with an actionable message.
    pub(crate) fn capture_unavailable(&self, error: &str) {
        self.set_effective_device("");
        self.status("error", None, Some("device_unusable"), Some(error));
    }

    /// One per-recording loss report; never alters the recording/status state.
    pub(crate) fn overflow(&self, raw: crate::audio::raw::RawCaptureOverflow, pending: u64) {
        if raw.capture_chunks == 0 && raw.pipeline_events == 0 && pending == 0 {
            return;
        }
        self.stderr(format!(
            "{LOG_PREFIX} WARNING: captured audio was dropped (capture_chunks={}, pipeline_events={}, pending_frames={pending}); transcript may be incomplete",
            raw.capture_chunks, raw.pipeline_events,
        ));
        self.send(RuntimeEvent::Worker(WorkerEvent {
            event: "audio_overflow".to_owned(),
            state: None,
            payload: serde_json::json!({
                "event": "audio_overflow",
                "capture_chunks_dropped": raw.capture_chunks,
                "pipeline_events_dropped": raw.pipeline_events,
                "pending_frames_dropped": pending,
            }),
        }));
    }

    fn send(&self, event: RuntimeEvent) {
        self.enqueue(event);
        self.notify_repaint();
    }

    fn enqueue(&self, event: RuntimeEvent) {
        let _ = self
            .tx
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .send(event);
    }

    pub(super) fn notify_repaint(&self) {
        if let Some(notifier) = self.repaint_notifier.as_ref() {
            notifier();
        }
    }
}

/// Build the `status` worker event the UI and `--json-events` consumers
/// already understand.
pub(crate) fn audio_status_event(
    state: &str,
    audio_device: Option<&str>,
    reason: Option<&str>,
    error: Option<&str>,
) -> RuntimeEvent {
    let mut payload = serde_json::Map::new();
    payload.insert("event".to_owned(), serde_json::Value::from("status"));
    payload.insert("state".to_owned(), serde_json::Value::from(state));
    if let Some(device) = audio_device {
        payload.insert("audio_device".to_owned(), serde_json::Value::from(device));
    }
    if let Some(reason) = reason {
        payload.insert("reason".to_owned(), serde_json::Value::from(reason));
    }
    if let Some(error) = error {
        payload.insert("error".to_owned(), serde_json::Value::from(error));
    }
    RuntimeEvent::Worker(WorkerEvent {
        event: "status".to_owned(),
        state: Some(state.to_owned()),
        payload: serde_json::Value::Object(payload),
    })
}

#[cfg(test)]
#[path = "capture_status_tests.rs"]
mod tests;
