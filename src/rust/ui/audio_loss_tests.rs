//! Ordinary-view regressions for the recording's one-shot audio-loss notice.

use super::test_support::test_app;
use super::{log_view_text, runtime_log_cards, AppSettings, LogViewMode, RuntimeLogCardKind};
use crate::runtime::{RuntimeEvent, WorkerEvent};
use serde_json::json;

const LOSS_WARNING: &str = "[rust-session-audio] WARNING: captured audio was dropped (capture_chunks=2, pipeline_events=3, pending_frames=5); transcript may be incomplete";

#[test]
fn audio_loss_warning_survives_structured_utterance_suppression_in_both_views() {
    let log = format!(
        "{LOSS_WARNING}\n[inject] strategy: paste text=\"partial preview\"\n[utterance] {{\"text\":\"final words\"}}"
    );
    for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
        let cards = runtime_log_cards(&log, mode);
        assert_eq!(cards.len(), 2, "one warning and one utterance in {mode:?}");
        assert_eq!(cards[0].kind, RuntimeLogCardKind::HealthFair);
        assert_eq!(cards[0].badge, "HealthFair");
        assert!(cards[0].title.contains("transcript may be incomplete"));
        assert!(cards[0].title.contains("capture_chunks=2"));
        assert!(cards[0].title.contains("pipeline_events=3"));
        assert!(cards[0].title.contains("pending_frames=5"));
        assert_eq!(cards[0].detail, "Recording audio loss");
        assert_eq!(cards[1].title, "final words");
    }
    assert!(log_view_text(&log, LogViewMode::Diagnostic).contains(LOSS_WARNING));
    // Copy in Minimal mode must remain dictated text, not diagnostics.
    assert_eq!(log_view_text(&log, LogViewMode::Minimal), "final words");
}

#[test]
fn unrelated_capture_logs_do_not_become_audio_loss_cards() {
    for line in [
        "[rust-session-audio] capture closed",
        "[rust-session-audio] WARNING: another diagnostic",
        "[rust-session-audio] captured audio was dropped (debug only)",
        "[another-worker] WARNING: captured audio was dropped (unknown)",
    ] {
        for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
            assert!(runtime_log_cards(line, mode).is_empty(), "{line}");
        }
        assert!(log_view_text(line, LogViewMode::Diagnostic).is_empty());
    }
}

fn hidden_tick(app: &mut super::WhisperDictateApp) {
    let ctx = eframe::egui::Context::default();
    let mut frame = eframe::Frame::_new_kittest();
    eframe::App::logic(app, &ctx, &mut frame);
}

fn isolated_app() -> super::WhisperDictateApp {
    let mut app = test_app(AppSettings::default());
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app
}

#[test]
fn runtime_loss_warning_is_visible_once_without_changing_live_state() {
    let mut app = isolated_app();
    app.pipeline_stage = Some("recording");
    app.pipeline_preview = Some("spoken preview".to_owned());
    app.last_worker_status_state = "recording".to_owned();
    app.device_error = Some("existing device failure".to_owned());
    app.active_audio_device = "USB mic".to_owned();
    app.last_runtime_error = Some("existing injection failure".to_owned());
    app.last_injection_failed = true;
    app.runtime_error_revision = 7;
    app.supervisor
        .send_event_for_tests(RuntimeEvent::Stderr(LOSS_WARNING.to_owned()));
    app.supervisor
        .send_event_for_tests(RuntimeEvent::Worker(WorkerEvent {
            event: "audio_overflow".to_owned(),
            state: None,
            payload: json!({
                "capture_chunks_dropped": 2,
                "pipeline_events_dropped": 3,
                "pending_frames_dropped": 5,
            }),
        }));
    hidden_tick(&mut app);

    for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
        let cards = app.runtime_log_cache.cards(mode);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, RuntimeLogCardKind::HealthFair);
        assert!(cards[0].title.contains("transcript may be incomplete"));
    }
    assert!(app
        .runtime_log_cache
        .text(LogViewMode::Diagnostic)
        .contains(LOSS_WARNING));
    assert_eq!(app.pipeline_stage, Some("recording"));
    assert_eq!(app.pipeline_preview.as_deref(), Some("spoken preview"));
    assert_eq!(app.last_worker_status_state, "recording");
    assert_eq!(app.active_audio_device, "USB mic");
    assert_eq!(app.device_error.as_deref(), Some("existing device failure"));
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("existing injection failure")
    );
    assert!(app.last_injection_failed);
    assert_eq!(app.runtime_error_revision, 7);
    assert_eq!(app.runtime_state, crate::runtime::RuntimeState::Stopped);

    let parsed = app.runtime_log_cache.stats().1;
    hidden_tick(&mut app);
    assert_eq!(app.runtime_log_cache.stats().1, parsed);
    assert_eq!(app.runtime_log_cache.cards(LogViewMode::Minimal).len(), 1);

    // A later structured transcript rebuilds the cache's views. The warning
    // must remain exactly once, rather than being suppressed as stage noise.
    app.supervisor
        .send_event_for_tests(RuntimeEvent::Worker(WorkerEvent {
            event: "utterance".to_owned(),
            state: None,
            payload: json!({"text": "completed transcript"}),
        }));
    hidden_tick(&mut app);
    for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
        let cards = app.runtime_log_cache.cards(mode);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].kind, RuntimeLogCardKind::HealthFair);
        assert_eq!(cards[1].title, "completed transcript");
        assert_eq!(cards, runtime_log_cards(&app.runtime_log, mode));
    }
    assert_eq!(
        app.runtime_log_cache.text(LogViewMode::Minimal),
        "completed transcript"
    );
}

#[test]
fn healthy_runtime_utterance_has_no_audio_loss_warning() {
    let mut app = isolated_app();
    app.supervisor
        .send_event_for_tests(RuntimeEvent::Worker(WorkerEvent {
            event: "utterance".to_owned(),
            state: None,
            payload: json!({"text": "healthy recording"}),
        }));
    hidden_tick(&mut app);
    for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
        let cards = app.runtime_log_cache.cards(mode);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "healthy recording");
        assert_ne!(cards[0].kind, RuntimeLogCardKind::HealthFair);
    }
    assert!(!app.runtime_log.contains("transcript may be incomplete"));
}

#[cfg(feature = "audio-capture")]
#[test]
fn stalled_pipeline_counts_reach_the_ui_through_the_capture_reporter() {
    use crate::audio::{raw::pipeline_event_channel, PipelineEvent};
    use crate::runtime::capture_status::CaptureReporter;
    use std::sync::{mpsc, Arc, RwLock};

    let (producer, receiver) = pipeline_event_channel(2);
    // A deliberately undrained real queue evicts exactly its oldest frame.
    for value in [0.25, 0.5, 0.75] {
        producer
            .try_send_latest(PipelineEvent::Frame(vec![value]))
            .unwrap();
    }
    drop(producer);
    let losses = receiver.overflow_snapshot();
    assert_eq!(losses.pipeline_events, 1);
    let (tx, events) = mpsc::channel();
    let reporter = CaptureReporter::new(tx, None, Arc::new(RwLock::new("USB mic".to_owned())));
    reporter.overflow(losses, 0);
    let mut app = isolated_app();
    let reported: Vec<_> = events.try_iter().collect();
    assert_eq!(reported.len(), 2, "one stderr warning and one JSON event");
    for event in reported {
        app.supervisor.send_event_for_tests(event);
    }
    hidden_tick(&mut app);
    for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
        let cards = app.runtime_log_cache.cards(mode);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, RuntimeLogCardKind::HealthFair);
        assert!(cards[0].title.contains("pipeline_events=1"));
        assert!(cards[0].title.contains("transcript may be incomplete"));
    }

    let (healthy_producer, healthy_receiver) = pipeline_event_channel(2);
    healthy_producer
        .try_send_latest(PipelineEvent::Frame(vec![0.5]))
        .unwrap();
    drop(healthy_producer);
    reporter.overflow(healthy_receiver.overflow_snapshot(), 0);
    assert!(
        events.try_recv().is_err(),
        "later healthy recording stays quiet"
    );
}
