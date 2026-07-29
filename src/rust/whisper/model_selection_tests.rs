//! Tests for explicit catalog-model selection.

use super::*;

#[test]
fn selected_model_wins_when_multiple_catalog_models_are_downloaded() {
    let selected = select_downloaded_model(Some("large-v3"), |entry| {
        matches!(entry.name, "large-v3-turbo" | "large-v3")
    })
    .unwrap()
    .expect("selected model");
    assert_eq!(selected.name, "large-v3");
}

#[test]
fn selected_model_must_exist_and_be_downloaded() {
    let missing = select_downloaded_model(Some("large-v3"), |_| false).unwrap_err();
    assert!(missing.to_string().contains("models download large-v3"));

    let unknown = select_downloaded_model(Some("not-a-model"), |_| true).unwrap_err();
    assert!(unknown.to_string().contains("unknown Whisper model"));
}
