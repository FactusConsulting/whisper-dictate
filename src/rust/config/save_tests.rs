use serde_json::{Map, Value};

use super::AppSettings;

#[test]
fn unrelated_save_preserves_an_absent_language_key() {
    let mut object: Map<String, Value> =
        serde_json::from_value(serde_json::json!({ "ui_theme": "dark" })).unwrap();

    AppSettings::default().apply_to_object(&mut object);

    assert!(
        !object.contains_key("lang"),
        "an unrelated save must not turn an absent language into an explicit Auto override"
    );
}

#[test]
fn clearing_an_existing_language_persists_explicit_null() {
    let mut object: Map<String, Value> =
        serde_json::from_value(serde_json::json!({ "lang": "da" })).unwrap();

    AppSettings::default().apply_to_object(&mut object);

    assert_eq!(object.get("lang"), Some(&Value::Null));
}
