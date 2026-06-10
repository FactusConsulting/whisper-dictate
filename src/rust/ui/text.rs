//! UI localization: the language enum plus the static English/Danish strings
//! used across the desktop shell and settings tabs.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum UiLanguageMode {
    English,
    Danish,
}

impl UiLanguageMode {
    fn from_raw(raw: &str) -> Self {
        match raw {
            "da" => Self::Danish,
            _ => Self::English,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum UiTextKey {
    SidebarSubtitle,
    Log,
    Speech,
    Quality,
    Dictionary,
    Output,
    Post,
    Profiles,
    Status,
    Backend,
    Model,
    Compute,
    Task,
    Start,
    Stop,
    LiveDictation,
    LogOutput,
    Copy,
    Clear,
    SaveSettings,
    SaveSettingsDirty,
    ReloadConfig,
    ResetPage,
    UnsavedChanges,
    SettingsSaved,
    UiLanguage,
    English,
    Danish,
    UiTheme,
    Dark,
    Light,
    Minimal,
    Diagnostic,
    Debug,
    InstallRepair,
    Doctor,
    Session,
    Stopped,
    Starting,
    Running,
    RuntimeStatus,
    NoDictationOutputYet,
    PushToTalk,
    Toggle,
}

impl UiTextKey {
    fn label(self, language: UiLanguageMode) -> &'static str {
        match language {
            UiLanguageMode::English => match self {
                UiTextKey::SidebarSubtitle => "Rust control surface",
                UiTextKey::Log => "Dictation",
                UiTextKey::Speech => "Speech",
                UiTextKey::Quality => "Quality",
                UiTextKey::Dictionary => "Dictionary",
                UiTextKey::Output => "Output",
                UiTextKey::Post => "Post",
                UiTextKey::Profiles => "Profiles",
                UiTextKey::Status => "Status",
                UiTextKey::Backend => "Backend",
                UiTextKey::Model => "Model",
                UiTextKey::Compute => "Compute",
                UiTextKey::Task => "Task",
                UiTextKey::Start => "Start",
                UiTextKey::Stop => "Stop",
                UiTextKey::LiveDictation => "Live dictation",
                UiTextKey::LogOutput => "Log output",
                UiTextKey::Copy => "Copy",
                UiTextKey::Clear => "Clear",
                UiTextKey::SaveSettings => "Save settings",
                UiTextKey::SaveSettingsDirty => "Save settings *",
                UiTextKey::ReloadConfig => "Reload config",
                UiTextKey::ResetPage => "Reset page",
                UiTextKey::UnsavedChanges => "Unsaved changes",
                UiTextKey::SettingsSaved => "Settings saved",
                UiTextKey::UiLanguage => "UI language",
                UiTextKey::English => "English",
                UiTextKey::Danish => "Danish",
                UiTextKey::UiTheme => "UI theme",
                UiTextKey::Dark => "Dark",
                UiTextKey::Light => "Light",
                UiTextKey::Minimal => "Minimal",
                UiTextKey::Diagnostic => "Diagnostic",
                UiTextKey::Debug => "Debug",
                UiTextKey::InstallRepair => "Install/Repair",
                UiTextKey::Doctor => "Doctor",
                UiTextKey::Session => "Session",
                UiTextKey::Stopped => "Stopped",
                UiTextKey::Starting => "Starting",
                UiTextKey::Running => "Running",
                UiTextKey::RuntimeStatus => "Runtime status",
                UiTextKey::NoDictationOutputYet => "No dictation output yet",
                UiTextKey::PushToTalk => "Push-to-talk",
                UiTextKey::Toggle => "Toggle key",
            },
            UiLanguageMode::Danish => match self {
                UiTextKey::SidebarSubtitle => "Rust kontrolflade",
                UiTextKey::Log => "Diktering",
                UiTextKey::Speech => "Tale",
                UiTextKey::Quality => "Kvalitet",
                UiTextKey::Dictionary => "Ordbog",
                UiTextKey::Output => "Output",
                UiTextKey::Post => "Efterbehandling",
                UiTextKey::Profiles => "Profiler",
                UiTextKey::Status => "Status",
                UiTextKey::Backend => "Backend",
                UiTextKey::Model => "Model",
                UiTextKey::Compute => "Beregning",
                UiTextKey::Task => "Opgave",
                UiTextKey::Start => "Start",
                UiTextKey::Stop => "Stop",
                UiTextKey::LiveDictation => "Live diktering",
                UiTextKey::LogOutput => "Log output",
                UiTextKey::Copy => "Kopier",
                UiTextKey::Clear => "Ryd",
                UiTextKey::SaveSettings => "Gem settings",
                UiTextKey::SaveSettingsDirty => "Gem settings *",
                UiTextKey::ReloadConfig => "Genindlæs config",
                UiTextKey::ResetPage => "Nulstil side",
                UiTextKey::UnsavedChanges => "Ikke gemt",
                UiTextKey::SettingsSaved => "Settings gemt",
                UiTextKey::UiLanguage => "UI-sprog",
                UiTextKey::English => "Engelsk",
                UiTextKey::Danish => "Dansk",
                UiTextKey::UiTheme => "UI-tema",
                UiTextKey::Dark => "Mørk",
                UiTextKey::Light => "Lys",
                UiTextKey::Minimal => "Minimal",
                UiTextKey::Diagnostic => "Diagnostik",
                UiTextKey::Debug => "Debug",
                UiTextKey::InstallRepair => "Installer/Reparer",
                UiTextKey::Doctor => "Diagnose",
                UiTextKey::Session => "Session",
                UiTextKey::Stopped => "Stoppet",
                UiTextKey::Starting => "Starter",
                UiTextKey::Running => "Kører",
                UiTextKey::RuntimeStatus => "Runtime status",
                UiTextKey::NoDictationOutputYet => "Ingen diktering endnu",
                UiTextKey::PushToTalk => "Tale-tast",
                UiTextKey::Toggle => "Skiftetast",
            },
        }
    }
}

pub(in crate::ui) fn ui_text(raw_language: &str, key: UiTextKey) -> &'static str {
    key.label(UiLanguageMode::from_raw(raw_language))
}

pub(in crate::ui) fn runtime_state_label(state: RuntimeState, raw_language: &str) -> &'static str {
    match state {
        RuntimeState::Stopped => ui_text(raw_language, UiTextKey::Stopped),
        RuntimeState::Starting => ui_text(raw_language, UiTextKey::Starting),
        RuntimeState::Running => ui_text(raw_language, UiTextKey::Running),
    }
}
