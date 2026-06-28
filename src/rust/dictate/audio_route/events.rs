//! Worker-event emitters used by [`super::AudioRoute`].
//!
//! Split out of `audio_route/mod.rs` so the main file stays under the
//! AGENTS.md ~500 LOC modularity bar (Codex P2 #415 audio_route.rs:530).
//! These helpers are free functions rather than methods because neither
//! one reads route state -- they only need the message payload + the
//! writer -- so keeping them off the impl block also makes the unit
//! tests in `audio_route_tests.rs` reachable without spinning up a
//! whole `AudioRoute`.

use std::io::Write;

use serde_json::{Map, Value};

use super::RouteError;
use crate::dictate::events::{self, StatusEvent, WorkerStatus};

/// One-shot `status=recording capped=true recording_s=N` worker
/// event, mirroring `vp_capture_rust_stdin.py:212-224`'s
/// `_emit_worker_event("status", state="recording", capped=True,
/// recording_s=round(buffered_s, 1))`. `recording_s` is rounded to
/// one decimal to match Python.
pub(super) fn emit_capped_status<W: Write>(
    buffered_s: f64,
    writer: &mut W,
) -> Result<(), RouteError> {
    let mut extras = Map::new();
    extras.insert("capped".into(), Value::from(true));
    extras.insert("recording_s".into(), Value::from(round_to_1dp(buffered_s)));
    let event = StatusEvent {
        state: WorkerStatus::Recording,
        extras,
        ..StatusEvent::new(WorkerStatus::Recording)
    };
    events::emit_status(writer, &event)?;
    Ok(())
}

/// Emit the `status=capture_lost` worker line for a
/// [`crate::audio::PipelineEvent::DeviceError`]. We use the canonical
/// `WorkerStatus::CaptureLost` state -- the Rust UI dispatcher in
/// `src/rust/ui/app.rs` switches on `event=status` and handles
/// `state=capture_lost` specifically; an `event=error` line would
/// be parsed and then ignored. Codex P2 #415 audio_route.rs:358.
///
/// Swallows a writer I/O failure on purpose: the device error
/// itself is already the headline diagnostic the supervisor will
/// surface, and we shouldn't mask it behind a follow-up
/// "couldn't write the status line" failure.
pub(super) fn emit_device_error<W: Write>(message: &str, writer: &mut W) {
    let mut extras = Map::new();
    // The Rust UI's status logger (`src/rust/ui/worker_event.rs:12-24`)
    // forwards a fixed allowlist of fields onto the status card.
    // `reason` is on the list; `message` is NOT, so writing the
    // actionable text only under `message` would silently reduce
    // every mic/VAD failure to a generic "capture_lost" line in the
    // log. Put the text under `reason` (UI-consumed) and ALSO
    // duplicate it under `message` so any consumer that greps the
    // raw worker-event stream for the original field keeps working.
    // Codex P2 #415 audio_route.rs:409.
    extras.insert("reason".into(), Value::from(message));
    extras.insert("message".into(), Value::from(message));
    extras.insert("backend".into(), Value::from("rust-stdin"));
    let event = StatusEvent {
        state: WorkerStatus::CaptureLost,
        extras,
        ..StatusEvent::new(WorkerStatus::CaptureLost)
    };
    let _ = events::emit_status(writer, &event);
}

/// Round to one decimal place, matching Python's
/// `round(buffered_s, 1)` in `vp_capture_rust_stdin.py:222`. Kept
/// local rather than importing `session::wire::round2` to avoid
/// reaching into a sibling module's private helper for a one-liner.
fn round_to_1dp(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_to_1dp_matches_python_round_buffered_s() {
        assert_eq!(round_to_1dp(0.06), 0.1);
        assert_eq!(round_to_1dp(0.04), 0.0);
        assert_eq!(round_to_1dp(120.0), 120.0);
        assert_eq!(round_to_1dp(120.05), 120.1);
    }
}
