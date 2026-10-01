//! Regression tests for native terminal output schemas.

use super::*;

#[test]
fn json_ready_line_is_structured_runtime_output() {
    let value: serde_json::Value =
        serde_json::from_str(&ready_line(true, "f9", "register-hotkey")).unwrap();
    assert_eq!(value["kind"], "ready");
    assert_eq!(value["engine"], "rust");
    assert_eq!(value["chord"], "f9");
}

#[test]
#[cfg(feature = "audio-capture")]
fn json_output_preserves_the_per_recording_audio_loss_counters() {
    let (tx, rx) = std::sync::mpsc::channel();
    let reporter = crate::runtime::capture_status::CaptureReporter::new(
        tx,
        None,
        std::sync::Arc::new(std::sync::RwLock::new("USB mic".to_owned())),
    );
    reporter.overflow(
        crate::audio::raw::RawCaptureOverflow {
            capture_chunks: 1,
            pipeline_events: 2,
        },
        3,
    );
    let _ = rx.recv().unwrap();
    let value = event_json_value(&rx.recv().unwrap());
    assert_eq!(value["kind"], "worker");
    assert_eq!(value["event"], "audio_overflow");
    assert_eq!(value["payload"]["capture_chunks_dropped"], 1);
    assert_eq!(value["payload"]["pipeline_events_dropped"], 2);
    assert_eq!(value["payload"]["pending_frames_dropped"], 3);
}

#[test]
fn utterance_json_preserves_the_established_top_level_schema() {
    let event = RuntimeEvent::Worker(WorkerEvent {
        event: "utterance".to_owned(),
        state: None,
        payload: serde_json::json!({
            "event": "utterance",
            "text": "hej verden",
            "model": "large-v3-turbo",
            "recording_s": 1.25,
        }),
    });
    let value = event_json_value(&event);
    assert_eq!(value["event"], "utterance");
    assert_eq!(value["text"], "hej verden");
    assert_eq!(value["model"], "large-v3-turbo");
    assert_eq!(value["recording_s"], 1.25);
    assert!(value.get("payload").is_none());
}

#[test]
fn started_json_exposes_the_installed_hotkey_not_a_syntax_prediction() {
    let event = RuntimeEvent::Started {
        command: "native-rust".to_owned(),
        hotkey_driver: "win_registerhotkey".to_owned(),
        hotkey_chord: "pause".to_owned(),
    };
    let value = event_json_value(&event);
    assert_eq!(value["kind"], "started");
    assert_eq!(value["hotkey_driver"], "win_registerhotkey");
    assert_eq!(value["hotkey_chord"], "pause");
}

#[test]
fn json_worker_output_preserves_audio_recovery_states_and_reason() {
    for state in ["audio-fallback", "audio-recovered"] {
        let event = RuntimeEvent::Worker(WorkerEvent {
            event: "status".to_owned(),
            state: Some(state.to_owned()),
            payload: serde_json::json!({
                "event": "status",
                "state": state,
                "audio_device": "System default",
            }),
        });
        let value = event_json_value(&event);
        assert_eq!(value["kind"], "worker");
        assert_eq!(value["event"], "status");
        assert_eq!(value["state"], state);
        assert_eq!(value["payload"]["audio_device"], "System default");
    }

    let event = RuntimeEvent::Worker(WorkerEvent {
        event: "status".to_owned(),
        state: Some("error".to_owned()),
        payload: serde_json::json!({
            "event": "status",
            "state": "error",
            "reason": "device_unusable",
            "audio_device": "Configured microphone",
        }),
    });
    let value = event_json_value(&event);
    assert_eq!(value["kind"], "worker");
    assert_eq!(value["state"], "error");
    assert_eq!(value["payload"]["reason"], "device_unusable");
}
