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
