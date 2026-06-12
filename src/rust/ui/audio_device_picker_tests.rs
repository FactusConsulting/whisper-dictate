//! Tests for the Microphone picker's option building and selected-label logic.

use super::test_support::test_app;
use super::*;

#[test]
fn microphone_options_always_lead_with_system_default() {
    let app = test_app(AppSettings::default());
    let options = app.microphone_options();
    assert_eq!(options[0].0, "");
    assert_eq!(options[0].1, "(System default)");
}

#[test]
fn microphone_options_include_refreshed_devices() {
    let mut app = test_app(AppSettings::default());
    app.audio_device_options = vec!["Yeti".to_owned(), "Webcam Mic".to_owned()];
    let options = app.microphone_options();
    let values: Vec<&str> = options.iter().map(|(value, _)| value.as_str()).collect();
    assert_eq!(values, vec!["", "Yeti", "Webcam Mic"]);
}

#[test]
fn microphone_options_preserve_saved_device_absent_from_list() {
    let mut app = test_app(AppSettings {
        audio_device: "Offline USB Mic".to_owned(),
        ..AppSettings::default()
    });
    app.audio_device_options = vec!["Yeti".to_owned()];
    let options = app.microphone_options();
    let saved = options
        .iter()
        .find(|(value, _)| value == "Offline USB Mic")
        .expect("saved device must remain selectable");
    assert_eq!(saved.1, "Offline USB Mic (saved)");
}

#[test]
fn microphone_options_do_not_duplicate_saved_device_already_listed() {
    let mut app = test_app(AppSettings {
        audio_device: "Yeti".to_owned(),
        ..AppSettings::default()
    });
    app.audio_device_options = vec!["Yeti".to_owned()];
    let options = app.microphone_options();
    let yeti_count = options.iter().filter(|(value, _)| value == "Yeti").count();
    assert_eq!(yeti_count, 1);
}

#[test]
fn changing_device_clears_stale_test_result() {
    // A finished test result belongs to the previously-selected device. Picking a
    // different microphone must clear it so the old ✓/✗ doesn't stay pinned next
    // to the new device.
    let mut app = test_app(AppSettings {
        audio_device: "Yeti".to_owned(),
        ..AppSettings::default()
    });
    app.device_test_result = Some(Ok(DeviceTestDisplay {
        outcome: DeviceTestOutcome::Works,
        endpoint: Some("wasapi".to_owned()),
        samplerate: Some(16000),
        resampled: false,
        reason: None,
    }));
    let device_before = app.settings.audio_device.clone();
    // Simulate the combo selecting a different device this frame.
    app.settings.audio_device = "Webcam Mic".to_owned();
    app.clear_device_test_result_if_device_changed(&device_before);
    assert!(app.device_test_result.is_none());
}

#[test]
fn unchanged_device_keeps_test_result() {
    // Re-rendering without a device change must NOT discard the result.
    let mut app = test_app(AppSettings {
        audio_device: "Yeti".to_owned(),
        ..AppSettings::default()
    });
    app.device_test_result = Some(Ok(DeviceTestDisplay {
        outcome: DeviceTestOutcome::Works,
        endpoint: Some("wasapi".to_owned()),
        samplerate: Some(16000),
        resampled: false,
        reason: None,
    }));
    let device_before = app.settings.audio_device.clone();
    app.clear_device_test_result_if_device_changed(&device_before);
    assert!(app.device_test_result.is_some());
}

#[test]
fn dynamic_selected_label_prefers_display_then_value_then_empty() {
    let options = vec![
        (String::new(), "(System default)".to_owned()),
        ("Yeti".to_owned(), "Yeti".to_owned()),
    ];
    assert_eq!(dynamic_selected_label("", &options), "(System default)");
    assert_eq!(dynamic_selected_label("Yeti", &options), "Yeti");
    // A value not in the options falls back to the raw value.
    assert_eq!(dynamic_selected_label("Other", &options), "Other");
}
