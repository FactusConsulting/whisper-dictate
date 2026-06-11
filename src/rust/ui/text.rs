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
    Recording,
    Ready,
    Log,
    Speech,
    Quality,
    Dictionary,
    Output,
    Post,
    Profiles,
    System,
    SystemMaintenance,
    SystemAppearance,
    SystemDisplay,
    SystemFeedback,
    SystemIntegration,
    DictationView,
    ConfigFile,
    UseDefaultPath,
    PostOn,
    PostOff,
    Status,
    Backend,
    Model,
    Compute,
    Task,
    Start,
    Stop,
    LiveDictation,
    DictationOutput,
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
    QualityGroupAllBackends,
    QualityGroupWhisper,
    QualityGroupParakeet,
    Dictation,
    SpeechGroupWhisper,
    SpeechGroupParakeet,
    SpeechGroupOnline,
    SpeechGroupGeneral,
    Diagnostics,
    DiagnosticsOff,
    DiagnosticsBasic,
    DiagnosticsVerbose,
    DiagnosticsHelp,
    UpdateAvailable,
    UpdateAvailableHover,
    SystemUpdates,
    UpdateCheck,
    UpdateCheckHelp,
    UpdateCheckInterval,
    UpdateCheckIntervalHelp,
    /// The word "Range" used as the prefix of the numeric-field range hint
    /// (e.g. "Range: 1–10."). Localized so a Danish help string isn't followed
    /// by an English "Range:".
    Range,
    /// Inline feedback when the typed hotkey chord is accepted.
    HotkeyValid,
    /// Inline feedback prefix when the hotkey chord is empty.
    HotkeyEmpty,
    /// Inline feedback prefix when a `+`-separated token is blank.
    HotkeyEmptyToken,
    /// Inline feedback prefix for an unknown token; followed by the token.
    HotkeyUnknownToken,
    /// Inline feedback prefix for a repeated token; followed by the token.
    HotkeyDuplicateToken,
    /// Expandable reference line: label for the accepted modifier tokens.
    HotkeyRefModifiers,
    /// Expandable reference line: label for the accepted named/function keys.
    HotkeyRefKeys,
}

impl UiTextKey {
    fn label(self, language: UiLanguageMode) -> &'static str {
        match language {
            UiLanguageMode::English => match self {
                UiTextKey::Recording => "Recording",
                UiTextKey::Ready => "Ready",
                UiTextKey::Log => "Dictation",
                UiTextKey::Speech => "Speech",
                UiTextKey::Quality => "Quality",
                UiTextKey::Dictionary => "Dictionary",
                UiTextKey::Output => "Output",
                UiTextKey::Post => "Post",
                UiTextKey::Profiles => "Profiles",
                UiTextKey::System => "System",
                UiTextKey::SystemMaintenance => "Maintenance",
                UiTextKey::SystemAppearance => "Appearance",
                UiTextKey::SystemDisplay => "Display",
                UiTextKey::SystemFeedback => "Feedback",
                UiTextKey::SystemIntegration => "Integration",
                UiTextKey::DictationView => "Dictation view",
                UiTextKey::ConfigFile => "Config file",
                UiTextKey::UseDefaultPath => "Use default path",
                UiTextKey::PostOn => "Post on",
                UiTextKey::PostOff => "Post off",
                UiTextKey::Status => "Status",
                UiTextKey::Backend => "Backend",
                UiTextKey::Model => "Model",
                UiTextKey::Compute => "Compute",
                UiTextKey::Task => "Task",
                UiTextKey::Start => "Start",
                UiTextKey::Stop => "Stop",
                UiTextKey::LiveDictation => "Live dictation",
                UiTextKey::DictationOutput => "Dictation output",
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
                UiTextKey::QualityGroupAllBackends => "All backends",
                UiTextKey::QualityGroupWhisper => "Whisper",
                UiTextKey::QualityGroupParakeet => "Parakeet",
                UiTextKey::Dictation => "Dictation",
                UiTextKey::SpeechGroupWhisper => "Local Whisper",
                UiTextKey::SpeechGroupParakeet => "Local NVIDIA Parakeet",
                UiTextKey::SpeechGroupOnline => "Cloud STT",
                UiTextKey::SpeechGroupGeneral => "General",
                UiTextKey::Diagnostics => "Diagnostics",
                UiTextKey::DiagnosticsOff => "Off",
                UiTextKey::DiagnosticsBasic => "Basic",
                UiTextKey::DiagnosticsVerbose => "Verbose",
                UiTextKey::DiagnosticsHelp => {
                    "How much diagnostic output the worker prints. \
                    Off = none. \
                    Basic = a concise per-utterance health line (microphone level/SNR \
                    + model confidence + warnings when something looks off). \
                    Verbose = Basic plus the startup effective-configuration dump and \
                    per-segment speech-to-text/dictionary detail. \
                    Set the Dictation view to \"Debug\" to see the raw lines in the log."
                }
                UiTextKey::UpdateAvailable => "available",
                UiTextKey::UpdateAvailableHover => {
                    "A newer version has been published. Update with:\n\
                    choco upgrade whisper-dictate --source=whisper-dictate -y\n\
                    or update via winget / the installer from the project releases."
                }
                UiTextKey::SystemUpdates => "Updates",
                UiTextKey::UpdateCheck => "Check for updates",
                UiTextKey::UpdateCheckHelp => {
                    "Periodically check whether a newer version has been published and show a \
                    discreet \"update available\" badge next to the version in the sidebar. \
                    PRIVACY: this only fetches the public version list from GitHub (github.io) \
                    and sends NO data, telemetry, or identifiers anywhere. \
                    Also settable via the VOICEPI_UPDATE_CHECK environment variable. \
                    Skipped automatically when \"Local only\" is enabled."
                }
                UiTextKey::UpdateCheckInterval => "Update check interval (minutes)",
                UiTextKey::UpdateCheckIntervalHelp => {
                    "How often to poll the public version list, in minutes (default 15, \
                    minimum 5). Also settable via VOICEPI_UPDATE_CHECK_INTERVAL_MINUTES."
                }
                UiTextKey::Range => "Range",
                UiTextKey::HotkeyValid => "Valid hotkey",
                UiTextKey::HotkeyEmpty => "Hotkey is empty",
                UiTextKey::HotkeyEmptyToken => "Empty key between '+' separators",
                UiTextKey::HotkeyUnknownToken => "Unknown key",
                UiTextKey::HotkeyDuplicateToken => "Duplicate key",
                UiTextKey::HotkeyRefModifiers => "Modifiers",
                UiTextKey::HotkeyRefKeys => "Keys",
            },
            UiLanguageMode::Danish => match self {
                UiTextKey::Recording => "Optager",
                UiTextKey::Ready => "Klar",
                UiTextKey::Log => "Diktering",
                UiTextKey::Speech => "Tale",
                UiTextKey::Quality => "Kvalitet",
                UiTextKey::Dictionary => "Ordbog",
                UiTextKey::Output => "Output",
                UiTextKey::Post => "Efterbehandling",
                UiTextKey::Profiles => "Profiler",
                UiTextKey::System => "System",
                UiTextKey::SystemMaintenance => "Vedligehold",
                UiTextKey::SystemAppearance => "Udseende",
                UiTextKey::SystemDisplay => "Visning",
                UiTextKey::SystemFeedback => "Feedback",
                UiTextKey::SystemIntegration => "Integration",
                UiTextKey::DictationView => "Dikteringsvisning",
                UiTextKey::ConfigFile => "Config-fil",
                UiTextKey::UseDefaultPath => "Brug standardsti",
                UiTextKey::PostOn => "Post til",
                UiTextKey::PostOff => "Post fra",
                UiTextKey::Status => "Status",
                UiTextKey::Backend => "Backend",
                UiTextKey::Model => "Model",
                UiTextKey::Compute => "Beregning",
                UiTextKey::Task => "Opgave",
                UiTextKey::Start => "Start",
                UiTextKey::Stop => "Stop",
                UiTextKey::LiveDictation => "Live diktering",
                UiTextKey::DictationOutput => "Dikteringsoutput",
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
                UiTextKey::QualityGroupAllBackends => "Alle backends",
                UiTextKey::QualityGroupWhisper => "Whisper",
                UiTextKey::QualityGroupParakeet => "Parakeet",
                UiTextKey::Dictation => "Diktering",
                UiTextKey::SpeechGroupWhisper => "Lokal Whisper",
                UiTextKey::SpeechGroupParakeet => "Lokal NVIDIA Parakeet",
                UiTextKey::SpeechGroupOnline => "Cloud STT",
                UiTextKey::SpeechGroupGeneral => "Generelt",
                UiTextKey::Diagnostics => "Diagnostik",
                UiTextKey::DiagnosticsOff => "Fra",
                UiTextKey::DiagnosticsBasic => "Basis",
                UiTextKey::DiagnosticsVerbose => "Udførlig",
                UiTextKey::DiagnosticsHelp => {
                    "Hvor meget diagnostik arbejderen skriver. \
                    Fra = ingen. \
                    Basis = en kort sundhedslinje pr. diktering (mikrofonniveau/SNR \
                    + modellens sikkerhed + advarsler hvis noget ser galt ud). \
                    Udførlig = Basis plus konfigurationsdump ved opstart og \
                    detaljer pr. segment for tale-til-tekst/ordbog. \
                    Sæt Dikterings-visningen til \"Debug\" for at se de rå linjer i loggen."
                }
                UiTextKey::UpdateAvailable => "tilgængelig",
                UiTextKey::UpdateAvailableHover => {
                    "En nyere version er udgivet. Opdater med:\n\
                    choco upgrade whisper-dictate --source=whisper-dictate -y\n\
                    eller opdater via winget / installeren fra projektets releases."
                }
                UiTextKey::SystemUpdates => "Opdateringer",
                UiTextKey::UpdateCheck => "Søg efter opdateringer",
                UiTextKey::UpdateCheckHelp => {
                    "Tjek med jævne mellemrum om en nyere version er udgivet, og vis et \
                    diskret \"opdatering tilgængelig\"-mærke ved versionen i sidepanelet. \
                    PRIVATLIV: dette henter kun den offentlige versionsliste fra GitHub \
                    (github.io) og sender INGEN data, telemetri eller identifikatorer nogen \
                    steder. Kan også sættes via miljøvariablen VOICEPI_UPDATE_CHECK. \
                    Springes automatisk over når \"Kun lokalt\" er slået til."
                }
                UiTextKey::UpdateCheckInterval => "Interval for opdateringstjek (minutter)",
                UiTextKey::UpdateCheckIntervalHelp => {
                    "Hvor ofte den offentlige versionsliste tjekkes, i minutter (standard 15, \
                    minimum 5). Kan også sættes via VOICEPI_UPDATE_CHECK_INTERVAL_MINUTES."
                }
                UiTextKey::Range => "Interval",
                UiTextKey::HotkeyValid => "Gyldig genvejstast",
                UiTextKey::HotkeyEmpty => "Genvejstast er tom",
                UiTextKey::HotkeyEmptyToken => "Tom tast mellem '+'-skilletegn",
                UiTextKey::HotkeyUnknownToken => "Ukendt tast",
                UiTextKey::HotkeyDuplicateToken => "Gentaget tast",
                UiTextKey::HotkeyRefModifiers => "Modifikatorer",
                UiTextKey::HotkeyRefKeys => "Taster",
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
