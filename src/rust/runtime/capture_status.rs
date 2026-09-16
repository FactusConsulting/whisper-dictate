//! Runtime status reporting for the push-to-talk microphone lifecycle.
//!
//! Every capture diagnostic flows through [`CaptureReporter`] so the UI gets
//! the same `[worker-event]` status contract as before #323:
//! `audio-fallback`, `audio-recovered`, and `error` with
//! `reason="device_unusable"`.

#![cfg(feature = "audio-capture")]
// The production caller (`rust_session_audio`) needs the full backend set.
#![cfg_attr(
    not(all(feature = "whisper-rs-local", feature = "rust-injection")),
    allow(dead_code)
)]

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};

use crate::runtime::{RepaintNotifier, RuntimeEvent, WorkerEvent};

/// Prefix every capture log line carries so a user grepping a log can pin
/// the source.
pub(crate) const LOG_PREFIX: &str = "[rust-session-audio]";

/// Effective-device label published while the OS default input is used in
/// place of a configured microphone.
pub(crate) const SYSTEM_DEFAULT_LABEL: &str = "System default";

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

    /// Clear the effective device and raise the persistent
    /// `device_unusable` banner with an actionable message.
    pub(crate) fn capture_unavailable(&self, error: &str) {
        self.set_effective_device("");
        self.status("error", None, Some("device_unusable"), Some(error));
    }

    fn send(&self, event: RuntimeEvent) {
        let _ = self
            .tx
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .send(event);
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
