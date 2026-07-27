//! Tests for [`super::rust_session_preview::runtime_channel_preview_sink`].
//!
//! Together these lock the Codex P1 #608
//! rust_session_real_backends.rs:372 contract: a preview emission dropped
//! into the sink must land on the runtime event channel as a
//! [`RuntimeEvent::Worker`] whose payload matches the shape the
//! subprocess-per-utterance path produces (so the UI's downstream
//! handling stays identical), AND the optional repaint notifier fires
//! per emission so a minimised-window install wakes up.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use super::runtime_channel_preview_sink;
use crate::dictate::PreviewEmission;
use crate::runtime::{RepaintNotifier, RuntimeEvent};

/// End-to-end delivery: dropping a [`PreviewEmission`] into the
/// [`runtime_channel_preview_sink`] must produce a
/// [`RuntimeEvent::Worker`] on the runtime channel whose payload shape
/// matches what the subprocess path (`parse_worker_event` over the
/// `[worker-event]` line) would have produced. This is the missing
/// piece the P1 finding named: pre-fix, previews went to stderr and the
/// in-process engine's channel-driven UI never saw them.
#[test]
fn runtime_channel_preview_sink_delivers_worker_event_to_channel() {
    let (tx, rx) = mpsc::channel::<RuntimeEvent>();
    let sink = runtime_channel_preview_sink(tx, None);

    sink(PreviewEmission {
        text: "hej verden".to_owned(),
        recording_s: 1.2345,
    });

    // Exactly one event must land on the channel -- the sink is
    // synchronous, so a queued receive must succeed without a wait.
    let event = rx
        .try_recv()
        .expect("preview sink must publish onto the runtime channel");
    let worker = match event {
        RuntimeEvent::Worker(w) => w,
        other => panic!("expected RuntimeEvent::Worker, got {other:?}"),
    };
    assert_eq!(worker.event, "status", "event name must be `status`");
    assert_eq!(
        worker.state.as_deref(),
        Some("preview"),
        "state must be `preview` so the UI's live-preview card triggers"
    );
    // Payload matches what `parse_worker_event` yields on the
    // subprocess path -- extras carry text_preview + recording_s
    // (2 dp rounding matches Python's round(x, 2)).
    assert_eq!(
        worker.payload.get("text_preview").and_then(|v| v.as_str()),
        Some("hej verden")
    );
    assert_eq!(
        worker.payload.get("recording_s").and_then(|v| v.as_f64()),
        Some(1.23)
    );
    assert!(
        rx.try_recv().is_err(),
        "sink must publish exactly one event per emission"
    );
}

/// The sink invokes the repaint notifier once per delivered event so
/// the egui UI wakes up to process the preview even when the window is
/// minimised (same rationale as `EventForwarder` -- see
/// `RuntimeSupervisor::repaint_notifier`).
#[test]
fn runtime_channel_preview_sink_calls_repaint_notifier_per_emission() {
    let (tx, _rx) = mpsc::channel::<RuntimeEvent>();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    let notifier: RepaintNotifier = Arc::new(move || {
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });

    let sink = runtime_channel_preview_sink(tx, Some(notifier));
    sink(PreviewEmission {
        text: "one".to_owned(),
        recording_s: 0.5,
    });
    sink(PreviewEmission {
        text: "two".to_owned(),
        recording_s: 1.0,
    });

    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "notifier must fire once per emission"
    );
}
