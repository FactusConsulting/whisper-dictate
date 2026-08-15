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
