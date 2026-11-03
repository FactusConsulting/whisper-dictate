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
    Settings,
    Output,
    Post,
    Profiles,
    System,
    SystemMaintenance,
    SystemStartup,
    AutostartRuntime,
    AutostartRuntimeHelp,
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
    CompactModel,
    CompactProfile,
    DefaultProfile,
    NotConfigured,
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
    /// Sidebar Simple/Advanced settings-visibility toggle: the "Simple" option.
    SettingsModeSimple,
    /// Sidebar Simple/Advanced settings-visibility toggle: the "Advanced" option.
    SettingsModeAdvanced,
    /// Hover text for the sidebar Simple/Advanced toggle.
    SettingsModeHelp,
    Minimal,
    Diagnostic,
    Debug,
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
    Dictation,
    SpeechGroupWhisper,
    SpeechGroupOnline,
    SpeechGroupGeneral,
    Diagnostics,
    DiagnosticsOff,
    DiagnosticsBasic,
    DiagnosticsVerbose,
    DiagnosticsTrace,
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
    /// Inline feedback when the chord syntax is accepted.
    HotkeyValid,
    /// Inline feedback prefix when the hotkey chord is empty.
    HotkeyEmpty,
    /// Inline feedback prefix when a `+`-separated token is blank.
    HotkeyEmptyToken,
    /// Inline feedback prefix for an unknown token; followed by the token.
    HotkeyUnknownToken,
    /// Inline feedback prefix for a repeated token; followed by the token.
    HotkeyDuplicateToken,
    /// Capability warning for a known but non-portable key.
    HotkeyWarningUnsupported,
    /// Capability warning for the Windows low-level fallback path.
    HotkeyWarningWindowsFallback,
    /// Preflight found a concrete listener that can install the chord.
    HotkeyInstallable,
    /// Concrete listener/chord installed by the running runtime.
    HotkeyInstalled,
    /// Reference line label for native modifier tokens.
    HotkeyRefModifiers,
    /// Reference line label for native trigger tokens.
    HotkeyRefKeys,
    /// Reference line label for native listener limits.
    HotkeyRefLimits,
    /// Reference line describing native listener limits.
    HotkeyRefLimitsText,
    /// Badge label for the per-utterance health card graded "perfect".
    HealthPerfect,
    /// Badge label for the per-utterance health card graded "good".
    HealthGood,
    /// Badge label for the per-utterance health card graded "fair".
    HealthFair,
    /// Badge label for the per-utterance health card graded "poor" (unusable).
    HealthPoor,
    /// "Refresh devices" button next to the Microphone picker.
    MicRefresh,
    /// Hover/help text for the Microphone "Refresh devices" button.
    MicRefreshHelp,
    /// "Test" button next to the Microphone picker.
    MicTest,
    /// Hover/help text for the Microphone "Test" button.
    MicTestHelp,
    /// In-flight label while the microphone test runs ("Testing…").
    MicTesting,
    /// ✓ result line: the selected microphone opened cleanly.
    MicTestWorks,
    /// ⚠ result PREFIX: works, but via a fallback path; followed by the detail
    /// (e.g. "via DirectSound (48 kHz, resampled)").
    MicTestWorksVia,
    /// ✗ result PREFIX: the microphone cannot be used; followed by the reason.
    MicTestCannot,
    /// The "resampled" qualifier inside the ⚠ caveat detail.
    MicTestResampled,
    /// Banner heading shown when the worker reports the selected mic is unusable.
    DeviceUnusableTitle,
    /// Banner heading shown when this process was refused push-to-talk because
    /// another whisper-dictate process already owns the chord.
    HotkeyConflictTitle,
    /// "Run benchmark" button in the System tab's Maintenance cluster.
    RunBenchmark,
    /// Hover/help text for the "Run benchmark" button.
    RunBenchmarkHelp,
    /// Hover prefix on the update badge when the install method has a copyable
    /// upgrade command; followed by the command on its own line. Tells the user a
    /// click copies it.
    UpdateCopyCommandHover,
    /// Hover prefix on the update badge when a click opens a release page
    /// (portable builds, and prerelease offers for package managers that carry
    /// finals only); followed by the URL.
    UpdateOpenReleaseHover,
    /// Transient confirmation shown after the upgrade command is copied.
    UpdateCommandCopied,
    /// Hover prefix on the update badge for a Chocolatey install; followed by
    /// the command the elevated console runs.
    UpdateRunChocoHover,
    /// Hover prefix on the update badge for a winget install; followed by the
    /// command the console runs.
    UpdateRunWingetHover,
    /// Hover prefix on the update badge for an installer install; followed by
    /// the setup executable's download URL.
    UpdateRunInstallerHover,
    /// Title of the update confirmation dialog.
    UpdateConfirmTitle,
    /// Confirmation dialog warning that the app closes during the update.
    UpdateConfirmClosesApp,
    /// Confirmation dialog detail for a Chocolatey update.
    UpdateConfirmChoco,
    /// Confirmation dialog detail for a winget update.
    UpdateConfirmWinget,
    /// Confirmation dialog detail for an installer download.
    UpdateConfirmInstaller,
    /// Confirmation dialog button that runs the update.
    UpdateConfirmButton,
    /// Confirmation dialog button that closes it without updating.
    UpdateCancel,
    /// Badge suffix while the installer downloads; followed by the progress.
    UpdateDownloading,
    /// Badge suffix after the update console or setup wizard started.
    UpdateStarted,
    /// Badge hover once an updater has started: further clicks do nothing.
    UpdateStartedHover,
    /// Prefix of the error line shown under the badge.
    UpdateFailed,
    /// Error shown when local-only mode blocks an update.
    UpdateBlockedLocalOnly,
    /// System-tray icon hover tooltip — no worker running (grey dot).
    TrayTipNotRunning,
    /// System-tray icon hover tooltip — worker ready, mic idle (green dot).
    TrayTipReady,
    /// System-tray icon hover tooltip — microphone actively capturing (red dot).
    TrayTipRecording,
    /// System-tray icon hover tooltip — transcribing/processing/starting (amber dot).
    TrayTipProcessing,
    /// System-tab checkbox label: opt in to receiving release-candidate updates.
    UpdateIncludePrereleases,
    /// Help text for the release-candidate opt-in checkbox.
    UpdateIncludePrereleasesHelp,
    CompactIdle,
    CompactStarting,
    CompactRecording,
    CompactRecordingProgress,
    CompactTranscribing,
    CompactPostProcessing,
    CompactInjecting,
    CompactError,
    LastTranscript,
    Reinject,
    Retry,
    LeaveCompact,
}

impl UiTextKey {
    fn label(self, language: UiLanguageMode) -> &'static str {
        match language {
            UiLanguageMode::English => self.label_english(),
            UiLanguageMode::Danish => self.label_danish(),
        }
    }

    /// The English copy for every [`UiTextKey`].
    fn label_english(self) -> &'static str {
        match self {
            UiTextKey::Recording => "Recording",
            UiTextKey::Ready => "Ready",
            UiTextKey::Log => "Dictation",
            UiTextKey::Speech => "Speech",
            UiTextKey::Quality => "Quality",
            UiTextKey::Dictionary => "Dictionary",
            UiTextKey::Settings => "Settings",
            UiTextKey::Output => "Output",
            UiTextKey::Post => "Post",
            UiTextKey::Profiles => "Profiles",
            UiTextKey::System => "System",
            UiTextKey::SystemMaintenance => "Maintenance",
            UiTextKey::SystemStartup => "Startup",
            UiTextKey::AutostartRuntime => "Start dictation on launch",
            UiTextKey::AutostartRuntimeHelp => {
                "Start the dictation runtime automatically when this app opens, using the \
                 saved settings, so you do not have to press Start. This does NOT start the \
                 app itself when you log in, and it does not open the microphone -- capture \
                 still only happens while push-to-talk is held. If the saved settings are \
                 not usable yet (a local model that is not downloaded, or a cloud provider \
                 with no API key) the reason is written to the log and nothing is started. \
                 A failed start is reported in the log exactly as a manual Start would be \
                 and is never retried."
            }
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
            UiTextKey::CompactModel => "model",
            UiTextKey::CompactProfile => "profile",
            UiTextKey::DefaultProfile => "Default profile",
            UiTextKey::NotConfigured => "Not configured",
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
            UiTextKey::SettingsModeSimple => "Simple",
            UiTextKey::SettingsModeAdvanced => "Advanced",
            UiTextKey::SettingsModeHelp => {
                "Simple shows only the essential settings; Advanced shows everything. \
                Your choice is remembered."
            }
            UiTextKey::Minimal => "Minimal",
            UiTextKey::Diagnostic => "Diagnostic",
            UiTextKey::Debug => "Debug",
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
            UiTextKey::Dictation => "Dictation",
            UiTextKey::SpeechGroupWhisper => "Local Whisper",
            UiTextKey::SpeechGroupOnline => "Cloud STT",
            UiTextKey::SpeechGroupGeneral => "General",
            UiTextKey::Diagnostics => "Diagnostics",
            UiTextKey::DiagnosticsOff => "Off",
            UiTextKey::DiagnosticsBasic => "Basic",
            UiTextKey::DiagnosticsVerbose => "Verbose",
            UiTextKey::DiagnosticsTrace => "Trace",
            UiTextKey::DiagnosticsHelp => {
                "How much native runtime diagnostic output is recorded. \
                Off = none. Basic = lifecycle and failures. \
                Verbose = runtime decisions, settings reloads, and action flow. \
                Trace = high-volume input, environment-key, capture, and teardown flow. \
                Trace can expose sensitive keyboard timing; use it only while troubleshooting. \
                Set the Dictation view to \"Debug\" to see the raw lines in the log."
            }
            UiTextKey::UpdateAvailable => "available",
            UiTextKey::UpdateAvailableHover => "A newer version has been published.",
            UiTextKey::SystemUpdates => "Updates",
            UiTextKey::UpdateCheck => "Check for updates",
            UiTextKey::UpdateCheckHelp => {
                "Periodically check whether a newer version has been published and show a \
                discreet \"update available\" badge next to the version in the sidebar. \
                PRIVACY: this only fetches the public version list from GitHub (github.io) \
                and sends NO data, telemetry, or identifiers anywhere. An update is only \
                downloaded when you click the badge and confirm. \
                Also settable via the VOICEPI_UPDATE_CHECK environment variable. \
                Skipped automatically when \"Local only\" is enabled."
            }
            UiTextKey::UpdateCheckInterval => "Update check interval (minutes)",
            UiTextKey::UpdateCheckIntervalHelp => {
                "How often to poll the public version list, in minutes (default 15, \
                minimum 5). Also settable via VOICEPI_UPDATE_CHECK_INTERVAL_MINUTES."
            }
            UiTextKey::Range => "Range",
            UiTextKey::HotkeyValid => "Valid syntax",
            UiTextKey::HotkeyEmpty => "Hotkey is empty",
            UiTextKey::HotkeyEmptyToken => "Empty key between '+' separators",
            UiTextKey::HotkeyUnknownToken => "Unknown key",
            UiTextKey::HotkeyDuplicateToken => "Duplicate key",
            UiTextKey::HotkeyWarningUnsupported => "Unsupported by the selected listener",
            UiTextKey::HotkeyWarningWindowsFallback => {
                "Fallback/focus risk; run the guided test. Preflight driver"
            }
            UiTextKey::HotkeyInstallable => "Installable, not yet verified. Preflight driver",
            UiTextKey::HotkeyInstalled => "Installed listener",
            UiTextKey::HotkeyRefModifiers => "Native modifiers",
            UiTextKey::HotkeyRefKeys => "Native triggers",
            UiTextKey::HotkeyRefLimits => "Limits",
            UiTextKey::HotkeyRefLimitsText => {
                "F13+, navigation, media, and lock keys are not supported by every native listener. Letter/digit triggers are Windows-only and are not accepted by the cross-platform UI. Windows uses the fallback listener for side-specific, modifier-only, or multi-trigger chords."
            }
            UiTextKey::HealthPerfect => "Perfect",
            UiTextKey::HealthGood => "Good",
            UiTextKey::HealthFair => "Fair",
            UiTextKey::HealthPoor => "Unusable",
            UiTextKey::MicRefresh => "Refresh devices",
            UiTextKey::MicRefreshHelp => {
                "Run the worker to list available microphones. The result populates the \
                picker; it does not load a model or start dictation."
            }
            UiTextKey::MicTest => "Test",
            UiTextKey::MicTestHelp => {
                "Dry-run open the selected microphone (resolve it and try the same \
                WASAPI/DirectSound/MME backends capture uses, recording no audio) so you \
                can confirm it works before starting dictation. Does not load a model."
            }
            UiTextKey::MicTesting => "Testing…",
            UiTextKey::MicTestWorks => "Works",
            UiTextKey::MicTestWorksVia => "Works via",
            UiTextKey::MicTestCannot => "Cannot be used",
            UiTextKey::MicTestResampled => "resampled",
            UiTextKey::DeviceUnusableTitle => "Microphone unavailable",
            UiTextKey::HotkeyConflictTitle => "Push-to-talk is in use by another window",
            UiTextKey::RunBenchmark => "Run benchmark",
            UiTextKey::RunBenchmarkHelp => {
                "Run the golden benchmark corpus (benchmark/corpus.json) through the \
                configured backend and write per-item results plus an overall \
                summary (pass count, average WER and CER) to the log. Runs in the \
                background — it loads the model and processes the whole corpus, so it \
                can take a while. Blocked while another background task runs."
            }
            UiTextKey::UpdateCopyCommandHover => "Click to copy the upgrade command:",
            UiTextKey::UpdateOpenReleaseHover => {
                "Click to open the release page and download the new version:"
            }
            UiTextKey::UpdateCommandCopied => "Copied!",
            UiTextKey::UpdateRunChocoHover => {
                "Click to update now. Runs this command in an administrator PowerShell window:"
            }
            UiTextKey::UpdateRunWingetHover => {
                "Click to update now. Runs this command in a PowerShell window:"
            }
            UiTextKey::UpdateRunInstallerHover => {
                "Click to update now. Downloads this installer, verifies its SHA-256 \
                checksum, and opens the setup wizard:"
            }
            UiTextKey::UpdateConfirmTitle => "Update whisper-dictate",
            UiTextKey::UpdateConfirmClosesApp => {
                "whisper-dictate closes during the update. Unsaved settings are lost."
            }
            UiTextKey::UpdateConfirmChoco => {
                "Windows asks for administrator rights, then a PowerShell window runs \
                Chocolatey. whisper-dictate starts again when the update finishes."
            }
            UiTextKey::UpdateConfirmWinget => {
                "A PowerShell window runs winget. whisper-dictate starts again when the \
                update finishes."
            }
            UiTextKey::UpdateConfirmInstaller => {
                "The installer is downloaded from GitHub and its SHA-256 checksum is \
                verified. The setup wizard then opens and offers to start the new version."
            }
            UiTextKey::UpdateConfirmButton => "Update now",
            UiTextKey::UpdateCancel => "Cancel",
            UiTextKey::UpdateDownloading => "downloading",
            UiTextKey::UpdateStarted => "update started",
            UiTextKey::UpdateStartedHover => {
                "An update window is already running, so clicking does nothing. If it was \
                closed without updating, restart whisper-dictate to try again."
            }
            UiTextKey::UpdateFailed => "Update failed:",
            UiTextKey::UpdateBlockedLocalOnly => {
                "Local only is on, so nothing was downloaded. Turn off Local only to update."
            }
            UiTextKey::TrayTipNotRunning => "whisper-dictate — not started",
            UiTextKey::TrayTipReady => "whisper-dictate — ready",
            UiTextKey::TrayTipRecording => "whisper-dictate — recording",
            UiTextKey::TrayTipProcessing => "whisper-dictate — processing",
            UiTextKey::UpdateIncludePrereleases => "Include release candidates",
            UiTextKey::UpdateIncludePrereleasesHelp => {
                "Also notify about release candidates (pre-releases like \
                1.10.0-rc.1) when checking for updates. RCs are early test \
                builds offered before a final release. Off by default — leave \
                it off for stable-only updates. Update them with \
                \"choco upgrade whisper-dictate --prerelease\" or by downloading \
                the matching installer from the release page. Also settable via \
                the VOICEPI_UPDATE_INCLUDE_PRERELEASES environment variable."
            }
            UiTextKey::CompactIdle => "Idle",
            UiTextKey::CompactStarting => "Starting…",
            UiTextKey::CompactRecording => "Recording",
            UiTextKey::CompactRecordingProgress => "Recording…",
            UiTextKey::CompactTranscribing => "Transcribing…",
            UiTextKey::CompactPostProcessing => "Post-processing…",
            UiTextKey::CompactInjecting => "Injecting…",
            UiTextKey::CompactError => "Error",
            UiTextKey::LastTranscript => "Last transcript",
            UiTextKey::Reinject => "Reinject",
            UiTextKey::Retry => "Retry",
            UiTextKey::LeaveCompact => "Leave compact mode",
        }
    }

    /// The Danish copy for every [`UiTextKey`].
    fn label_danish(self) -> &'static str {
        match self {
            UiTextKey::Recording => "Optager",
            UiTextKey::Ready => "Klar",
            UiTextKey::Log => "Diktering",
            UiTextKey::Speech => "Tale",
            UiTextKey::Quality => "Kvalitet",
            UiTextKey::Dictionary => "Ordbog",
            UiTextKey::Settings => "Indstillinger",
            UiTextKey::Output => "Output",
            UiTextKey::Post => "Efterbehandling",
            UiTextKey::Profiles => "Profiler",
            UiTextKey::System => "System",
            UiTextKey::SystemMaintenance => "Vedligehold",
            UiTextKey::SystemStartup => "Opstart",
            UiTextKey::AutostartRuntime => "Start diktering ved opstart",
            UiTextKey::AutostartRuntimeHelp => {
                "Start dikteringsmotoren automatisk, når denne app åbnes, med de gemte \
                 indstillinger, så du ikke behøver trykke Start. Det starter IKKE selve \
                 appen, når du logger ind, og det åbner ikke mikrofonen -- optagelse sker \
                 stadig kun, mens push-to-talk holdes nede. Hvis de gemte indstillinger \
                 ikke kan bruges endnu (en lokal model der ikke er hentet, eller en \
                 cloud-udbyder uden API-nøgle), skrives årsagen i loggen, og der startes \
                 ikke noget. En mislykket start rapporteres i loggen præcis som en manuel \
                 Start og forsøges aldrig igen."
            }
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
            UiTextKey::CompactModel => "model",
            UiTextKey::CompactProfile => "profil",
            UiTextKey::DefaultProfile => "Standardprofil",
            UiTextKey::NotConfigured => "Ikke konfigureret",
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
            UiTextKey::SettingsModeSimple => "Enkel",
            UiTextKey::SettingsModeAdvanced => "Avanceret",
            UiTextKey::SettingsModeHelp => {
                "Enkel viser kun de vigtigste indstillinger; Avanceret viser alt. \
                Dit valg huskes."
            }
            UiTextKey::Minimal => "Minimal",
            UiTextKey::Diagnostic => "Diagnostik",
            UiTextKey::Debug => "Debug",
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
            UiTextKey::Dictation => "Diktering",
            UiTextKey::SpeechGroupWhisper => "Lokal Whisper",
            UiTextKey::SpeechGroupOnline => "Cloud STT",
            UiTextKey::SpeechGroupGeneral => "Generelt",
            UiTextKey::Diagnostics => "Diagnostik",
            UiTextKey::DiagnosticsOff => "Fra",
            UiTextKey::DiagnosticsBasic => "Basis",
            UiTextKey::DiagnosticsVerbose => "Udførlig",
            UiTextKey::DiagnosticsTrace => "Trace",
            UiTextKey::DiagnosticsHelp => {
                "Hvor meget diagnostik den native runtime gemmer. \
                Fra = ingen. Basis = livscyklus og fejl. \
                Udførlig = runtime-beslutninger, genindlæsning af indstillinger og handlingsflow. \
                Trace = højvolumen-input, miljønøgler, optagelse og nedlukningsflow. \
                Trace kan afsløre følsom tastaturtiming; brug kun under fejlfinding. \
                Sæt Dikterings-visningen til \"Debug\" for at se de rå linjer i loggen."
            }
            UiTextKey::UpdateAvailable => "tilgængelig",
            UiTextKey::UpdateAvailableHover => "En nyere version er udgivet.",
            UiTextKey::SystemUpdates => "Opdateringer",
            UiTextKey::UpdateCheck => "Søg efter opdateringer",
            UiTextKey::UpdateCheckHelp => {
                "Tjek med jævne mellemrum om en nyere version er udgivet, og vis et \
                diskret \"opdatering tilgængelig\"-mærke ved versionen i sidepanelet. \
                PRIVATLIV: dette henter kun den offentlige versionsliste fra GitHub \
                (github.io) og sender INGEN data, telemetri eller identifikatorer nogen \
                steder. En opdatering hentes kun, når du klikker på mærket og bekræfter. \
                Kan også sættes via miljøvariablen VOICEPI_UPDATE_CHECK. \
                Springes automatisk over når \"Kun lokalt\" er slået til."
            }
            UiTextKey::UpdateCheckInterval => "Interval for opdateringstjek (minutter)",
            UiTextKey::UpdateCheckIntervalHelp => {
                "Hvor ofte den offentlige versionsliste tjekkes, i minutter (standard 15, \
                minimum 5). Kan også sættes via VOICEPI_UPDATE_CHECK_INTERVAL_MINUTES."
            }
            UiTextKey::Range => "Interval",
            UiTextKey::HotkeyValid => "Gyldig syntaks",
            UiTextKey::HotkeyEmpty => "Genvejstast er tom",
            UiTextKey::HotkeyEmptyToken => "Tom tast mellem '+'-skilletegn",
            UiTextKey::HotkeyUnknownToken => "Ukendt tast",
            UiTextKey::HotkeyDuplicateToken => "Gentaget tast",
            UiTextKey::HotkeyWarningUnsupported => "Ikke understøttet af den valgte lytter",
            UiTextKey::HotkeyWarningWindowsFallback => {
                "Fallback-/fokusrisiko; kør den guidede test. Forhåndstjekket driver"
            }
            UiTextKey::HotkeyInstallable => "Kan installeres, endnu ikke verificeret. Forhåndstjekket driver",
            UiTextKey::HotkeyInstalled => "Installeret lytter",
            UiTextKey::HotkeyRefModifiers => "Native modifikatorer",
            UiTextKey::HotkeyRefKeys => "Native taster",
            UiTextKey::HotkeyRefLimits => "Begrænsninger",
            UiTextKey::HotkeyRefLimitsText => {
                "F13+, navigation, medier og låsetaster understøttes ikke af alle native lyttere. Bogstav-/ciffertaster er kun til Windows og accepteres ikke i den tværplatforms UI. Windows bruger fallback-lytteren til sidebestemte, modifikator-only eller fler-taste-genveje."
            }
            UiTextKey::HealthPerfect => "Perfekt",
            UiTextKey::HealthGood => "God",
            UiTextKey::HealthFair => "Middel",
            UiTextKey::HealthPoor => "Ubrugelig",
            UiTextKey::MicRefresh => "Opdater enheder",
            UiTextKey::MicRefreshHelp => {
                "Kør workeren for at vise tilgængelige mikrofoner. Resultatet udfylder \
                listen; det indlæser ingen model og starter ikke diktering."
            }
            UiTextKey::MicTest => "Test",
            UiTextKey::MicTestHelp => {
                "Prøveåbn den valgte mikrofon (find den og prøv de samme \
                WASAPI/DirectSound/MME-backends som optagelsen bruger, uden at optage lyd), \
                så du kan bekræfte at den virker før diktering startes. Indlæser ingen model."
            }
            UiTextKey::MicTesting => "Tester…",
            UiTextKey::MicTestWorks => "Virker",
            UiTextKey::MicTestWorksVia => "Virker via",
            UiTextKey::MicTestCannot => "Kan ikke bruges",
            UiTextKey::MicTestResampled => "resamplet",
            UiTextKey::DeviceUnusableTitle => "Mikrofon utilgængelig",
            UiTextKey::HotkeyConflictTitle => "Tale-tasten bruges af et andet vindue",
            UiTextKey::RunBenchmark => "Kør benchmark",
            UiTextKey::RunBenchmarkHelp => {
                "Kør det gyldne benchmark-korpus (benchmark/corpus.json) gennem den \
                konfigurerede backend, og skriv resultater pr. element samt en samlet \
                opsummering (antal beståede, gennemsnitlig WER og CER) til loggen. \
                Kører i baggrunden — den indlæser modellen og behandler hele korpusset, \
                så det kan tage et stykke tid. Blokeret mens en anden baggrundsopgave kører."
            }
            UiTextKey::UpdateCopyCommandHover => "Klik for at kopiere opdateringskommandoen:",
            UiTextKey::UpdateOpenReleaseHover => {
                "Klik for at åbne udgivelsessiden og hente den nye version:"
            }
            UiTextKey::UpdateCommandCopied => "Kopieret!",
            UiTextKey::UpdateRunChocoHover => {
                "Klik for at opdatere nu. Kører denne kommando i et PowerShell-vindue \
                som administrator:"
            }
            UiTextKey::UpdateRunWingetHover => {
                "Klik for at opdatere nu. Kører denne kommando i et PowerShell-vindue:"
            }
            UiTextKey::UpdateRunInstallerHover => {
                "Klik for at opdatere nu. Henter denne installer, kontrollerer dens \
                SHA-256-kontrolsum og åbner installationsguiden:"
            }
            UiTextKey::UpdateConfirmTitle => "Opdatér whisper-dictate",
            UiTextKey::UpdateConfirmClosesApp => {
                "whisper-dictate lukkes under opdateringen. Ikke-gemte indstillinger går tabt."
            }
            UiTextKey::UpdateConfirmChoco => {
                "Windows beder om administratorrettigheder, og derefter kører Chocolatey \
                i et PowerShell-vindue. whisper-dictate starter igen, når opdateringen \
                er færdig."
            }
            UiTextKey::UpdateConfirmWinget => {
                "Et PowerShell-vindue kører winget. whisper-dictate starter igen, når \
                opdateringen er færdig."
            }
            UiTextKey::UpdateConfirmInstaller => {
                "Installeren hentes fra GitHub, og dens SHA-256-kontrolsum kontrolleres. \
                Derefter åbner installationsguiden og tilbyder at starte den nye version."
            }
            UiTextKey::UpdateConfirmButton => "Opdatér nu",
            UiTextKey::UpdateCancel => "Annullér",
            UiTextKey::UpdateDownloading => "henter",
            UiTextKey::UpdateStarted => "opdatering startet",
            UiTextKey::UpdateStartedHover => {
                "Et opdateringsvindue kører allerede, så et klik gør ingenting. Hvis det blev \
                lukket uden at opdatere, så genstart whisper-dictate for at prøve igen."
            }
            UiTextKey::UpdateFailed => "Opdateringen mislykkedes:",
            UiTextKey::UpdateBlockedLocalOnly => {
                "\"Kun lokalt\" er slået til, så intet blev hentet. Slå \"Kun lokalt\" fra \
                for at opdatere."
            }
            UiTextKey::TrayTipNotRunning => "whisper-dictate — ikke startet",
            UiTextKey::TrayTipReady => "whisper-dictate — klar",
            UiTextKey::TrayTipRecording => "whisper-dictate — optager",
            UiTextKey::TrayTipProcessing => "whisper-dictate — behandler",
            UiTextKey::UpdateIncludePrereleases => "Inkludér release candidates",
            UiTextKey::UpdateIncludePrereleasesHelp => {
                "Giv også besked om release candidates (pre-releases som \
                1.10.0-rc.1) ved tjek for opdateringer. RC'er er tidlige \
                testudgaver, der tilbydes før en endelig udgivelse. Slået fra \
                som standard — lad den være slået fra for kun stabile \
                opdateringer. Opdatér dem med \
                \"choco upgrade whisper-dictate --prerelease\" eller ved at \
                hente den tilsvarende installer fra udgivelsessiden. Kan også \
                sættes via miljøvariablen VOICEPI_UPDATE_INCLUDE_PRERELEASES."
            }
            UiTextKey::CompactIdle => "Ledig",
            UiTextKey::CompactStarting => "Starter…",
            UiTextKey::CompactRecording => "Optager",
            UiTextKey::CompactRecordingProgress => "Optager…",
            UiTextKey::CompactTranscribing => "Transskriberer…",
            UiTextKey::CompactPostProcessing => "Efterbehandler…",
            UiTextKey::CompactInjecting => "Indsætter…",
            UiTextKey::CompactError => "Fejl",
            UiTextKey::LastTranscript => "Seneste transskription",
            UiTextKey::Reinject => "Indsæt igen",
            UiTextKey::Retry => "Prøv igen",
            UiTextKey::LeaveCompact => "Forlad kompakt tilstand",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_candidate_optin_strings_present_en_and_da() {
        // EN label/help.
        assert_eq!(
            ui_text("en", UiTextKey::UpdateIncludePrereleases),
            "Include release candidates"
        );
        let en_help = ui_text("en", UiTextKey::UpdateIncludePrereleasesHelp);
        assert!(en_help.contains("release candidates"));
        assert!(en_help.contains("VOICEPI_UPDATE_INCLUDE_PRERELEASES"));
        assert!(en_help.contains("--prerelease"));

        // DA label/help (distinct from EN, so the localization is real).
        assert_eq!(
            ui_text("da", UiTextKey::UpdateIncludePrereleases),
            "Inkludér release candidates"
        );
        let da_help = ui_text("da", UiTextKey::UpdateIncludePrereleasesHelp);
        assert_ne!(da_help, en_help, "DA help must differ from EN");
        assert!(da_help.contains("VOICEPI_UPDATE_INCLUDE_PRERELEASES"));
        assert!(da_help.contains("--prerelease"));
    }

    #[test]
    fn health_grade_labels_present_and_distinct_en_and_da() {
        let keys = [
            UiTextKey::HealthPerfect,
            UiTextKey::HealthGood,
            UiTextKey::HealthFair,
            UiTextKey::HealthPoor,
        ];
        for lang in ["en", "da"] {
            let labels: Vec<&str> = keys.iter().map(|&key| ui_text(lang, key)).collect();
            // Each grade has a non-empty label...
            for label in &labels {
                assert!(!label.is_empty(), "empty {lang} health-grade label");
            }
            // ...and the four are distinct within a language.
            for i in 0..labels.len() {
                for j in (i + 1)..labels.len() {
                    assert_ne!(
                        labels[i], labels[j],
                        "{lang} health-grade labels must be distinct"
                    );
                }
            }
        }
        // Spot-check the exact catalogue strings (both languages).
        assert_eq!(ui_text("en", UiTextKey::HealthPerfect), "Perfect");
        assert_eq!(ui_text("en", UiTextKey::HealthGood), "Good");
        assert_eq!(ui_text("en", UiTextKey::HealthFair), "Fair");
        assert_eq!(ui_text("en", UiTextKey::HealthPoor), "Unusable");
        assert_eq!(ui_text("da", UiTextKey::HealthPerfect), "Perfekt");
        assert_eq!(ui_text("da", UiTextKey::HealthGood), "God");
        assert_eq!(ui_text("da", UiTextKey::HealthFair), "Middel");
        assert_eq!(ui_text("da", UiTextKey::HealthPoor), "Ubrugelig");
    }
}
