use super::*;

#[test]
fn ui_language_translates_primary_navigation_and_runtime_status() {
    assert_eq!(Tab::Log.label("en"), "Dictation");
    assert_eq!(Tab::Log.label("da"), "Diktering");
    assert_eq!(Tab::Speech.label("en"), "Speech");
    assert_eq!(Tab::Speech.label("da"), "Tale");
    assert_eq!(Tab::Dictionary.label("da"), "Ordbog");
    assert_eq!(ui_text("da", UiTextKey::LiveDictation), "Live diktering");
    assert_eq!(
        ui_text("en", UiTextKey::DictationOutput),
        "Dictation output"
    );
    assert_eq!(
        ui_text("da", UiTextKey::DictationOutput),
        "Dikteringsoutput"
    );
    assert_eq!(ui_text("da", UiTextKey::Copy), "Kopier");
    assert_eq!(ui_text("da", UiTextKey::InstallRepair), "Installer/Reparer");
    // System tab strings, both languages (also: Sonar new-code coverage).
    assert_eq!(Tab::System.label("en"), "System");
    assert_eq!(Tab::System.label("da"), "System");
    assert_eq!(ui_text("en", UiTextKey::SystemMaintenance), "Maintenance");
    assert_eq!(ui_text("da", UiTextKey::SystemMaintenance), "Vedligehold");
    assert_eq!(ui_text("en", UiTextKey::SystemAppearance), "Appearance");
    assert_eq!(ui_text("da", UiTextKey::SystemAppearance), "Udseende");
    assert_eq!(ui_text("en", UiTextKey::SystemDisplay), "Display");
    assert_eq!(ui_text("da", UiTextKey::SystemDisplay), "Visning");
    assert_eq!(ui_text("en", UiTextKey::SystemFeedback), "Feedback");
    assert_eq!(ui_text("da", UiTextKey::SystemFeedback), "Feedback");
    assert_eq!(ui_text("en", UiTextKey::SystemIntegration), "Integration");
    assert_eq!(ui_text("da", UiTextKey::SystemIntegration), "Integration");
    assert_eq!(ui_text("en", UiTextKey::DictationView), "Dictation view");
    assert_eq!(ui_text("da", UiTextKey::DictationView), "Dikteringsvisning");
    assert_eq!(ui_text("en", UiTextKey::ConfigFile), "Config file");
    assert_eq!(ui_text("da", UiTextKey::ConfigFile), "Config-fil");
    assert_eq!(ui_text("en", UiTextKey::PostOn), "Post on");
    assert_eq!(ui_text("da", UiTextKey::PostOn), "Post til");
    assert_eq!(ui_text("en", UiTextKey::PostOff), "Post off");
    assert_eq!(ui_text("da", UiTextKey::PostOff), "Post fra");
    assert_eq!(ui_text("da", UiTextKey::Doctor), "Diagnose");
    assert_eq!(ui_text("da", UiTextKey::Light), "Lys");
    assert_eq!(LogViewMode::Diagnostic.label("da"), "Diagnostik");
    assert_eq!(runtime_state_label(RuntimeState::Running, "da"), "Kører");
    // Quality scope-group headings added by the metrics-default-path PR.
    assert_eq!(
        ui_text("en", UiTextKey::QualityGroupAllBackends),
        "All backends"
    );
    assert_eq!(
        ui_text("da", UiTextKey::QualityGroupAllBackends),
        "Alle backends"
    );
    assert_eq!(ui_text("en", UiTextKey::QualityGroupWhisper), "Whisper");
    assert_eq!(ui_text("da", UiTextKey::QualityGroupWhisper), "Whisper");
    assert_eq!(ui_text("en", UiTextKey::QualityGroupParakeet), "Parakeet");
    assert_eq!(ui_text("da", UiTextKey::QualityGroupParakeet), "Parakeet");
    // UseDefaultPath key — used by the System tab "Use default path" button.
    assert_eq!(ui_text("en", UiTextKey::UseDefaultPath), "Use default path");
    assert_eq!(ui_text("da", UiTextKey::UseDefaultPath), "Brug standardsti");
}

#[test]
fn ui_language_falls_back_to_english_for_unknown_values() {
    assert_eq!(Tab::Quality.label("fr"), "Quality");
    assert_eq!(ui_text("fr", UiTextKey::UiLanguage), "UI language");
    assert_eq!(runtime_state_label(RuntimeState::Stopped, "fr"), "Stopped");
}
