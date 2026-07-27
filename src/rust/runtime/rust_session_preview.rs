//! In-process preview-sink adapter for the real-backend session.
//!
//! Extracted from [`super::rust_session_real_backends`] so both modules
//! stay under the AGENTS.md 500-LOC modularity limit (Codex P1 #608
//! preview.rs:457 companion split).
//!
//! # What this fixes (Codex P1 #608 rust_session_real_backends.rs:372)
//!
//! The pre-fix wiring passed [`crate::dictate::stderr_preview_sink`] into
//! the [`crate::dictate::PreviewEngine`]. That sink writes preview events
//! to the *process*'s stderr; the subprocess-per-utterance Python engine
//! recovers them by having the supervisor's stderr reader parse the
//! `[worker-event]` line back into a [`WorkerEvent`] and forward it onto
//! the runtime channel. The in-process Rust engine has no such reader
//! -- its UI consumes only events published on the [`RuntimeEvent`]
//! channel -- so preview lines written to stderr never made it to the
//! live pipeline card.
//!
//! [`runtime_channel_preview_sink`] instead pushes a
//! [`RuntimeEvent::Worker`] onto the runtime channel directly. The
//! `WorkerEvent` payload is byte-equivalent to the value
//! `runtime::process::parse_worker_event` would have reconstructed from
//! the stderr line, so the UI's downstream handling is identical
//! regardless of which sink the session was wired with.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::dictate::{build_preview_status, PreviewEmission, PreviewSink};
use crate::runtime::{RepaintNotifier, RuntimeEvent, WorkerEvent};

/// Build a [`PreviewSink`] that routes each preview into the in-process
/// runtime event channel as a [`RuntimeEvent::Worker`]. See module-level
/// docs for the pre/post-fix behaviour comparison.
///
/// - `tx` -- clone of the runtime event channel the sink publishes on.
/// - `repaint_notifier` -- optional egui repaint hook; fired once per
///   delivered event so a minimised-window install wakes up to process
///   the preview (matches the equivalent
///   `EventForwarder`-plus-notifier pattern already used for stderr
///   events -- see [`crate::runtime::supervisor::RuntimeSupervisor`]'s
///   `repaint_notifier` field for the "why").
pub(crate) fn runtime_channel_preview_sink(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> PreviewSink {
    Arc::new(move |emission: PreviewEmission| {
        let status = build_preview_status(&emission);
        // Materialise the same JSON shape `emit_status` writes (minus
        // the Python-parity ASCII escapes, which don't affect the
        // `Value` the UI sees). Keys land in `serde_json::Map`
        // alphabetically -- byte-equivalent to Python's
        // `sort_keys=True` after `parse_worker_event` has
        // deserialised.
        let mut payload = serde_json::Map::new();
        payload.insert("event".into(), serde_json::Value::from("status"));
        payload.insert(
            "state".into(),
            serde_json::Value::from(status.state.as_wire_str()),
        );
        for (key, value) in status.extras.iter() {
            if value.is_null() || key == "event" {
                continue;
            }
            payload.insert(key.clone(), value.clone());
        }
        let event = WorkerEvent {
            event: "status".to_owned(),
            state: Some(status.state.as_wire_str().to_owned()),
            payload: serde_json::Value::Object(payload),
        };
        let _ = tx.send(RuntimeEvent::Worker(event));
        if let Some(notifier) = repaint_notifier.as_ref() {
            notifier();
        }
    })
}

#[cfg(test)]
#[path = "rust_session_preview_tests.rs"]
mod tests;
