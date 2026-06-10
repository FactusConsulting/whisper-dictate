use super::*;

#[test]
fn ui_language_translates_primary_navigation_and_runtime_status() {
    assert_eq!(Tab::Log.label("en"), "Dictation");
    assert_eq!(Tab::Log.label("da"), "Diktering");
    assert_eq!(Tab::Speech.label("en"), "Speech");
    assert_eq!(Tab::Speech.label("da"), "Tale");
    assert_eq!(Tab::Dictionary.label("da"), "Ordbog");
    assert_eq!(ui_text("da", UiTextKey::LiveDictation), "Live diktering");
    assert_eq!(ui_text("en", UiTextKey::DictationOutput), "Dictation output");
    assert_eq!(ui_text("da", UiTextKey::DictationOutput), "Dikteringsoutput");
    assert_eq!(ui_text("da", UiTextKey::Copy), "Kopier");
    assert_eq!(ui_text("da", UiTextKey::InstallRepair), "Installer/Reparer");
    assert_eq!(ui_text("da", UiTextKey::Doctor), "Diagnose");
    assert_eq!(ui_text("da", UiTextKey::Light), "Lys");
    assert_eq!(LogViewMode::Diagnostic.label("da"), "Diagnostik");
    assert_eq!(runtime_state_label(RuntimeState::Running, "da"), "Kører");
}

#[test]
fn ui_language_falls_back_to_english_for_unknown_values() {
    assert_eq!(Tab::Quality.label("fr"), "Quality");
    assert_eq!(ui_text("fr", UiTextKey::UiLanguage), "UI language");
    assert_eq!(runtime_state_label(RuntimeState::Stopped, "fr"), "Stopped");
}
