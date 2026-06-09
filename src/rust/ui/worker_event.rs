//! Parsing helpers for the worker's JSON status/audio/utterance events and the
//! derived live audio-meter state.

use crate::runtime::{RuntimeState, WorkerEvent};

pub(in crate::ui) fn worker_status_log_line(event: &WorkerEvent) -> Option<String> {
    if event.event != "status" {
        return None;
    }
    let state = event.state.as_deref().unwrap_or("unknown");
    let mut line = format!("[worker] status={state}");
    for key in [
        "backend",
        "model",
        "device",
        "compute_type",
        "capture_backend",
        "capture_channels",
        "audio_device",
        "startup_ms",
        "first_audio",
        "recording_s",
    ] {
        if let Some(value) = worker_event_string(&event.payload, key) {
            line.push(' ');
            line.push_str(key);
            line.push('=');
            line.push_str(&value);
        }
    }
    Some(line)
}

pub(in crate::ui) fn worker_utterance_log_line(event: &WorkerEvent) -> Option<String> {
    if event.event != "utterance" {
        return None;
    }
    serde_json::to_string(&event.payload)
        .ok()
        .map(|payload| format!("[utterance] {payload}"))
}

pub(in crate::ui) fn worker_event_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    let value = payload.get(key)?;
    if let Some(raw) = value.as_str() {
        let raw = raw.trim();
        return (!raw.is_empty()).then(|| raw.to_owned());
    }
    if value.is_number() || value.is_boolean() {
        return Some(value.to_string());
    }
    None
}

pub(in crate::ui) fn worker_event_f32(payload: &serde_json::Value, key: &str) -> Option<f32> {
    let value = payload.get(key)?;
    if let Some(raw) = value.as_f64() {
        return Some(raw as f32);
    }
    value.as_str()?.trim().parse::<f32>().ok()
}

pub(in crate::ui) fn worker_event_bool(payload: &serde_json::Value, key: &str) -> Option<bool> {
    let value = payload.get(key)?;
    if let Some(raw) = value.as_bool() {
        return Some(raw);
    }
    value.as_str()?.trim().parse::<bool>().ok()
}

pub(in crate::ui) fn audio_capture_active_for_worker_state(state: &str) -> Option<bool> {
    match state {
        "recording" | "listening" => Some(true),
        "opening" | "ready" | "transcribing" | "loading_model" | "failed" => Some(false),
        _ => None,
    }
}

/// The live dictation-pipeline stage a worker status maps to (drives the
/// progress card), or `None` for non-pipeline states (idle/startup/error).
pub(in crate::ui) fn pipeline_stage_for_worker_state(state: &str) -> Option<&'static str> {
    match state {
        "recording" => Some("recording"),
        "transcribing" => Some("transcribing"),
        "post-processing" => Some("post-processing"),
        _ => None,
    }
}

pub(in crate::ui) fn audio_meter_level(
    live_level: f32,
    state: RuntimeState,
    capture_active: bool,
) -> f32 {
    if state == RuntimeState::Stopped || !capture_active {
        return 0.0;
    }
    live_level.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn worker_event_string_trims_and_coerces_scalars() {
        let payload = json!({
            "audio_device": "  Yeti  ",
            "blank": "   ",
            "channels": 2,
            "active": true,
            "level": 0.5,
            "nested": {"a": 1},
        });
        // String values are trimmed; blank-after-trim is dropped.
        assert_eq!(
            worker_event_string(&payload, "audio_device").as_deref(),
            Some("Yeti")
        );
        assert_eq!(worker_event_string(&payload, "blank"), None);
        // Numbers and bools render through their JSON form.
        assert_eq!(
            worker_event_string(&payload, "channels").as_deref(),
            Some("2")
        );
        assert_eq!(
            worker_event_string(&payload, "active").as_deref(),
            Some("true")
        );
        // Non-scalar/missing keys yield None.
        assert_eq!(worker_event_string(&payload, "nested"), None);
        assert_eq!(worker_event_string(&payload, "missing"), None);
    }

    #[test]
    fn worker_event_f32_accepts_numbers_and_numeric_strings() {
        let payload = json!({"a": -33.2, "b": "  0.42 ", "c": "nope", "d": true});
        assert_eq!(worker_event_f32(&payload, "a"), Some(-33.2));
        assert_eq!(worker_event_f32(&payload, "b"), Some(0.42));
        assert_eq!(worker_event_f32(&payload, "c"), None);
        assert_eq!(worker_event_f32(&payload, "d"), None);
        assert_eq!(worker_event_f32(&payload, "missing"), None);
    }

    #[test]
    fn worker_event_bool_accepts_bools_and_bool_strings() {
        let payload = json!({"a": true, "b": "false", "c": "TRUE", "d": "1", "e": " true "});
        assert_eq!(worker_event_bool(&payload, "a"), Some(true));
        assert_eq!(worker_event_bool(&payload, "b"), Some(false));
        // Surrounding whitespace is trimmed, matching the string/f32 parsers.
        assert_eq!(worker_event_bool(&payload, "e"), Some(true));
        // Only Rust's bool parser is accepted (case-sensitive, no "1"/"0").
        assert_eq!(worker_event_bool(&payload, "c"), None);
        assert_eq!(worker_event_bool(&payload, "d"), None);
        assert_eq!(worker_event_bool(&payload, "missing"), None);
    }
}
