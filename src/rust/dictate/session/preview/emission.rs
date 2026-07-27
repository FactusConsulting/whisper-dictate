//! Preview emission types + sinks.
//!
//! Split out of the pre-modularity-fix single-file `preview.rs` (Codex P1
//! #608 preview.rs:457) so the "what does a successful tick produce, and
//! where does it go" surface lives independently of the engine / state
//! machine. The engine module ([`super::engine`]) computes the payload
//! and hands it to a [`PreviewSink`]; how that sink transports the
//! payload (stderr line, in-process channel, unit-test capture) is
//! entirely this module's concern.

use std::io::Write;
use std::sync::Arc;

use crate::dictate::events::{emit_status, StatusEvent, WorkerStatus};

/// Payload the preview sink receives once per successful tick.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewEmission {
    /// Decoded text (already truncated to
    /// [`super::engine::PreviewEngineConfig::text_chars`]).
    pub text: String,
    /// Total captured audio at the moment of the tick, rounded to 2 dp
    /// (mirrors Python's `round(samples / capture_rate, 2)`).
    pub recording_s: f64,
}

/// Sink the preview worker calls once per emitted preview. The production
/// wiring on the subprocess-per-utterance path routes through
/// [`stderr_preview_sink`], which writes a `state="preview"` worker event
/// via [`emit_status`]; the in-process Rust engine wires a sink that
/// pushes a [`crate::runtime::RuntimeEvent::Worker`] onto the runtime
/// channel instead (see
/// [`crate::runtime::rust_session_real_backends::runtime_channel_preview_sink`]).
/// Tests capture the emissions into a `Vec`.
pub type PreviewSink = Arc<dyn Fn(PreviewEmission) + Send + Sync>;

/// Build the stderr preview sink: emits each preview as a
/// `state="preview"` worker event on stderr using the same
/// [`crate::dictate::events`] emitter every other worker event goes
/// through. Respects the `VOICEPI_WORKER_EVENTS` env-gate exactly like
/// the session's own emitter, so a supervisor that opted out sees no
/// preview lines either.
///
/// Historical / CLI paths that supervise a child process (or run the
/// engine without a runtime channel) still use this sink. The in-process
/// Rust engine wires [`crate::runtime::rust_session_real_backends::runtime_channel_preview_sink`]
/// instead so preview events reach the UI's `RuntimeEvent` channel
/// directly (Codex P1 #608 rust_session_real_backends.rs:372 -- the
/// stderr sink was invisible to the in-process UI, which does not read
/// stderr).
pub fn stderr_preview_sink() -> PreviewSink {
    Arc::new(|emission: PreviewEmission| {
        let event = build_preview_status(&emission);
        let mut stderr = std::io::stderr().lock();
        let _ = emit_status(&mut stderr, &event);
        let _ = stderr.flush();
    })
}

/// Build the `StatusEvent` a preview emits. Exposed so the production sink
/// and the unit tests share the exact same field-shape assembly -- if this
/// drifts, the UI's live preview card breaks.
pub fn build_preview_status(emission: &PreviewEmission) -> StatusEvent {
    let mut event = StatusEvent::new(WorkerStatus::Preview);
    event.extras.insert(
        "text_preview".into(),
        serde_json::Value::from(emission.text.clone()),
    );
    event.extras.insert(
        "recording_s".into(),
        serde_json::json!(round2(emission.recording_s)),
    );
    event
}

/// Truncate `text` to at most `chars` characters. Empty string when the
/// input has none. Kept local so the preview module does not pull in the
/// wider `text` helpers (which do more work -- Python's `_compact_text`
/// also normalises whitespace; the preview cap is a hard length limit).
pub(crate) fn truncate_chars(text: &str, chars: usize) -> String {
    if chars == 0 {
        return String::new();
    }
    let mut it = text.chars();
    let head: String = (&mut it).take(chars).collect();
    if it.next().is_some() {
        // There were more characters than the cap allowed; return the head.
        head
    } else {
        text.to_owned()
    }
}

/// Round to 2 decimal places, matching Python's `round(x, 2)`. Duplicated
/// from `wire.rs` so this module has no cross-module private dep.
pub(crate) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
