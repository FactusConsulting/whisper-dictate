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

#[test]
fn cached_edda_requires_explicit_selection_and_never_falls_back() {
    for name in ["edda-v0.2", "edda-v0.2-large"] {
        let only_edda = |entry: &ModelEntry| entry.name == name;
        assert!(select_downloaded_model(None, only_edda).unwrap().is_none());
        assert_eq!(
            select_downloaded_model(Some(name), only_edda)
                .unwrap()
                .unwrap()
                .fixed_language,
            Some("da")
        );
        let error =
            select_downloaded_model(Some(name), |entry| entry.name == "large-v3").unwrap_err();
        assert!(error
            .to_string()
            .contains(&format!("wd models download {name}")));
    }
}
