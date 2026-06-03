use anyhow::Result;
use eframe::egui;
use std::collections::BTreeMap;
use std::env;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::cloud_api::{check_cloud_api, CloudApiCheck};
use crate::config::{self, AppSettings};
use crate::dictionary;
use crate::runtime::{
    self, default_worker_command, doctor_command, install_command, run_capture, RuntimeEvent,
    RuntimeState, RuntimeSupervisor, WorkerCommand,
};
use crate::telemetry;

const GROQ_STT_BASE_URL: &str = "https://api.groq.com/openai/v1";
const GROQ_STT_MODEL: &str = "whisper-large-v3-turbo";
const GROQ_POST_MODEL: &str = "llama-3.1-8b-instant";
const GROQ_KEYS_URL: &str = "https://console.groq.com/keys";
const OPENAI_STT_BASE_URL: &str = "https://api.openai.com/v1";
const OPENAI_STT_MODEL: &str = "gpt-4o-mini-transcribe";
const OPENAI_POST_MODEL: &str = "gpt-4o-mini";
const OPENAI_KEYS_URL: &str = "https://platform.openai.com/api-keys";
const STT_API_KEY_ENV: &str = "VOICEPI_STT_API_KEY";
const POST_API_KEY_ENV: &str = "VOICEPI_POST_API_KEY";
const CREDENTIAL_SERVICE: &str = "whisper-dictate";
const WHISPER_MODELS: &[&str] = &[
    "large-v3-turbo",
    "large-v3",
    "medium",
    "small",
    "base",
    "tiny",
];
const GROQ_STT_MODELS: &[&str] = &[
    "whisper-large-v3-turbo",
    "whisper-large-v3",
    "distil-whisper-large-v3-en",
];
const OPENAI_STT_MODELS: &[&str] = &["gpt-4o-mini-transcribe", "gpt-4o-transcribe", "whisper-1"];
const GROQ_POST_MODELS: &[&str] = &[
    "llama-3.1-8b-instant",
    "llama-3.3-70b-versatile",
    "meta-llama/llama-4-scout-17b-16e-instruct",
    "qwen/qwen3-32b",
    "openai/gpt-oss-20b",
    "openai/gpt-oss-120b",
    "groq/compound-mini",
    "groq/compound",
];
const OPENAI_POST_MODELS: &[&str] = &["gpt-4o-mini", "gpt-4o", "gpt-4.1-mini"];
const PARAKEET_MODELS: &[&str] = &[
    "",
    "nvidia/parakeet-tdt-0.6b-v3",
    "nvidia/parakeet-tdt-1.1b",
    "nvidia/parakeet-tdt-0.6b-v2",
];
const DEFAULT_UI_TEXT_SCALE: f32 = 1.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SttBackendMode {
    Whisper,
    Parakeet,
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloudProvider {
    Groq,
    OpenAi,
}

impl CloudProvider {
    fn from_raw(raw: &str) -> Option<Self> {
        match raw {
            "groq" => Some(Self::Groq),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }

    fn from_settings(settings: &AppSettings) -> Self {
        if settings
            .stt_base_url
            .to_ascii_lowercase()
            .contains("api.groq.com")
        {
            Self::Groq
        } else {
            Self::OpenAi
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Groq => "groq",
            Self::OpenAi => "openai",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Groq => "Groq",
            Self::OpenAi => "OpenAI",
        }
    }

    fn base_url(self) -> &'static str {
        match self {
            Self::Groq => GROQ_STT_BASE_URL,
            Self::OpenAi => OPENAI_STT_BASE_URL,
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::Groq => GROQ_STT_MODEL,
            Self::OpenAi => OPENAI_STT_MODEL,
        }
    }

    fn model_options(self) -> &'static [&'static str] {
        match self {
            Self::Groq => GROQ_STT_MODELS,
            Self::OpenAi => OPENAI_STT_MODELS,
        }
    }

    fn key_url(self) -> &'static str {
        match self {
            Self::Groq => GROQ_KEYS_URL,
            Self::OpenAi => OPENAI_KEYS_URL,
        }
    }

    fn credential_user(self) -> &'static str {
        match self {
            Self::Groq => "stt-api-key:groq",
            Self::OpenAi => "stt-api-key:openai",
        }
    }
}

impl SttBackendMode {
    fn from_raw(raw: &str) -> Self {
        match raw {
            "parakeet" => Self::Parakeet,
            "openai" => Self::Cloud,
            _ => Self::Whisper,
        }
    }
}

pub fn run() -> Result<()> {
    runtime::cleanup_stale_desktop_processes();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 760.0])
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        &format!("whisper-dictate {}", runtime::version()),
        options,
        Box::new(|_cc| Ok(Box::new(WhisperDictateApp::default()))),
    )
    .map_err(|err| anyhow::anyhow!(err.to_string()))
}

struct WhisperDictateApp {
    selected_tab: Tab,
    runtime_state: RuntimeState,
    runtime_log: String,
    config_path: String,
    settings: AppSettings,
    saved_settings: AppSettings,
    settings_status: String,
    stt_api_key_input: String,
    saved_stt_api_key_input: String,
    stt_api_key_status: String,
    dictionary_preview: String,
    history_preview: String,
    metrics_preview: String,
    supervisor: RuntimeSupervisor,
    background_task: Option<Receiver<BackgroundTaskResult>>,
    background_task_label: Option<&'static str>,
}

impl Default for WhisperDictateApp {
    fn default() -> Self {
        let (settings, settings_status) = match config::load_settings() {
            Ok(settings) => (settings, String::new()),
            Err(err) => (
                AppSettings::default(),
                format!("Could not load config, using defaults: {err}"),
            ),
        };
        let provider = CloudProvider::from_settings(&settings);
        let (stt_api_key_input, saved_stt_api_key_input, stt_api_key_status) =
            load_stt_api_key_state(provider).unwrap_or_else(|err| {
                (
                    String::new(),
                    String::new(),
                    format!("Could not load API key: {err}"),
                )
            });
        Self {
            selected_tab: Tab::Runtime,
            runtime_state: RuntimeState::Stopped,
            runtime_log: "Rust UI ready. Start launches the Python dictation worker directly."
                .to_owned(),
            config_path: config::config_path().display().to_string(),
            saved_settings: settings.clone(),
            settings,
            settings_status,
            saved_stt_api_key_input,
            stt_api_key_input,
            stt_api_key_status,
            dictionary_preview: String::new(),
            history_preview: String::new(),
            metrics_preview: String::new(),
            supervisor: RuntimeSupervisor::new(),
            background_task: None,
            background_task_label: None,
        }
    }
}

struct BackgroundTaskResult {
    label: &'static str,
    command: String,
    stdout: String,
    stderr: String,
    success: bool,
    code: Option<i32>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Runtime,
    Core,
    Quality,
    Dictionary,
    Output,
    Profiles,
}

impl Tab {
    const ALL: [Tab; 6] = [
        Tab::Runtime,
        Tab::Core,
        Tab::Quality,
        Tab::Dictionary,
        Tab::Output,
        Tab::Profiles,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Runtime => "Runtime",
            Tab::Core => "Core",
            Tab::Quality => "Quality",
            Tab::Dictionary => "Dictionary",
            Tab::Output => "Output",
            Tab::Profiles => "Profiles",
        }
    }
}

impl eframe::App for WhisperDictateApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_runtime();
        self.poll_background_task();
        apply_ui_text_scale(ctx, &self.settings.ui_text_scale);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("whisper-dictate {}", runtime::version()));
                ui.separator();
                for tab in Tab::ALL {
                    ui.selectable_value(&mut self.selected_tab, tab, tab.label());
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.selected_tab {
            Tab::Runtime => self.runtime_tab(ui),
            Tab::Core => self.settings_panel(ui, Self::core_tab),
            Tab::Quality => self.settings_panel(ui, Self::quality_tab),
            Tab::Dictionary => self.settings_panel(ui, Self::dictionary_tab),
            Tab::Output => self.settings_panel(ui, Self::output_tab),
            Tab::Profiles => self.settings_panel(ui, Self::profiles_tab),
        });
    }
}

impl WhisperDictateApp {
    fn runtime_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Runtime");
        ui.horizontal(|ui| {
            if ui.button("Start").clicked() {
                self.start_runtime();
            }
            if ui.button("Stop").clicked() {
                self.stop_runtime();
            }
            if ui.button("Restart").clicked() {
                self.restart_runtime();
            }
            if ui.button("Doctor").clicked() {
                self.run_doctor();
            }
            if ui
                .add_enabled(
                    self.background_task.is_none(),
                    egui::Button::new("Install/Repair"),
                )
                .clicked()
            {
                self.run_install();
            }
            if ui.button("Clear").clicked() {
                self.runtime_log.clear();
            }
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(self.runtime_log.clone());
            }
            ui.separator();
            ui.label(format!("Status: {}", self.runtime_state.label()));
            if let Some(label) = self.background_task_label {
                ui.label(format!("Task: {label} running"));
            }
        });

        ui.separator();
        ui.label(format!("Config: {}", self.config_path));
        ui.add_space(8.0);
        ui.label("Runtime log");
        let height = (ui.available_height() - 8.0).max(240.0);
        let mut runtime_log_view = self.runtime_log.clone();
        let row_count = self.runtime_log.lines().count().max(28);
        egui::ScrollArea::both()
            .id_salt("runtime_log_scroll")
            .auto_shrink([false, false])
            .max_height(height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut runtime_log_view)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(row_count)
                        .id_salt("runtime_log_view")
                        .code_editor()
                        .desired_width(ui.available_width()),
                );
            });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui, body: fn(&mut Self, &mut egui::Ui)) {
        body(self, ui);
        ui.separator();
        let is_dirty = self.has_unsaved_settings();
        ui.horizontal(|ui| {
            let mut save_button = egui::Button::new(if is_dirty {
                egui::RichText::new("Save settings *").strong()
            } else {
                egui::RichText::new("Save settings")
            });
            if is_dirty {
                save_button = save_button.fill(ui.visuals().selection.bg_fill);
            }
            if ui
                .add_enabled(is_dirty, save_button)
                .on_hover_text("Save changed settings and any edited cloud API key.")
                .clicked()
            {
                self.save_settings();
            }
            if ui.button("Reload from disk").clicked() {
                self.reload_settings();
            }
            if is_dirty {
                ui.colored_label(ui.visuals().warn_fg_color, "Unsaved changes");
            }
            ui.label(format!("Config: {}", self.config_path));
        });
        if !self.settings_status.is_empty() {
            ui.label(&self.settings_status);
        }
    }

    fn core_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Core");
        let backend = SttBackendMode::from_raw(&self.settings.stt_backend);
        let mut provider_id = self.current_cloud_provider().id().to_owned();
        egui::Grid::new("core_settings")
            .num_columns(2)
            .show(ui, |ui| {
                combo_help(
                    ui,
                    "STT backend",
                    &mut self.settings.stt_backend,
                    &["whisper", "parakeet", "openai"],
                    "Choose the transcription engine: local Whisper, local NVIDIA Parakeet, or OpenAI-compatible cloud STT.",
                );
                ui.end_row();
                ui.strong("Local Whisper");
                ui.label("Used only when STT backend is whisper.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Whisper,
                    "Whisper model",
                    &mut self.settings.model,
                    WHISPER_MODELS,
                    "Local faster-whisper model used only with STT backend = whisper.",
                );
                ui.end_row();
                ui.strong("Local NVIDIA Parakeet");
                ui.label("Used only when STT backend is parakeet.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Parakeet,
                    "Parakeet model",
                    &mut self.settings.parakeet_model,
                    PARAKEET_MODELS,
                    "Local NVIDIA NeMo Parakeet model used only with STT backend = parakeet.",
                );
                ui.end_row();
                ui.strong("Cloud STT");
                ui.label("Used only when STT backend is openai/Groq.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT provider",
                    &mut provider_id,
                    &["groq", "openai"],
                    "OpenAI-compatible cloud provider. Groq uses Groq-hosted Whisper models; OpenAI uses OpenAI transcription models.",
                );
                if let Some(provider) = CloudProvider::from_raw(&provider_id) {
                    if provider != self.current_cloud_provider() {
                        self.set_cloud_provider(provider);
                    }
                }
                ui.end_row();
                let provider = self.current_cloud_provider();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT model",
                    &mut self.settings.stt_model,
                    provider.model_options(),
                    "Remote transcription model for the selected cloud provider.",
                );
                text_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT API URL",
                    &mut self.settings.stt_base_url,
                    "Base URL for OpenAI-compatible transcription APIs, for example Groq.",
                );
                text_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT timeout ms",
                    &mut self.settings.stt_timeout_ms,
                    "Network timeout for cloud transcription requests.",
                );
                password_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT API key",
                    &mut self.stt_api_key_input,
                    "Stored in the OS credential store and passed to the worker as VOICEPI_STT_API_KEY.",
                );
                ui.strong("Runtime");
                ui.label("Applies to local backends unless otherwise noted.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend != SttBackendMode::Cloud,
                    "Device",
                    &mut self.settings.device,
                    &["auto", "cuda", "cpu"],
                    "Local inference device. auto chooses CUDA when available, otherwise CPU.",
                );
                combo_enabled(
                    ui,
                    backend != SttBackendMode::Cloud,
                    "Compute type",
                    &mut self.settings.compute_type,
                    &["", "int8_float16", "float16", "bfloat16", "float32", "int8"],
                    "Local model precision/performance mode. Leave empty for backend default.",
                );
                combo_help(
                    ui,
                    "Language",
                    &mut self.settings.lang,
                    &["", "da", "en", "de", "fr", "sv", "nb", "nl", "es", "it"],
                    "Spoken language hint. Empty lets the backend autodetect when supported.",
                );
                text_help(
                    ui,
                    "Hotkey",
                    &mut self.settings.key,
                    "Hold-to-talk key or chord, for example ctrl_r or shift_l+ctrl_l.",
                );
                text_help(
                    ui,
                    "Quit key",
                    &mut self.settings.quit_key,
                    "Global key used to quit the worker after Quit count presses. Examples: esc, f12, q.",
                );
                text_help(
                    ui,
                    "Quit count",
                    &mut self.settings.quit_count,
                    "Number of consecutive quit-key presses required to stop the worker. 0 disables it.",
                );
                text_help(
                    ui,
                    "Quit window ms",
                    &mut self.settings.quit_window_ms,
                    "Maximum time window for consecutive quit-key presses.",
                );
            });
        if backend == SttBackendMode::Cloud {
            let provider = self.current_cloud_provider();
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{} API keys", provider.label()))
                    .clicked()
                {
                    match open_url(provider.key_url()) {
                        Ok(()) => {
                            self.stt_api_key_status =
                                format!("Opened {} API keys page.", provider.label());
                        }
                        Err(err) => {
                            self.stt_api_key_status =
                                format!("Could not open {} API keys page: {err}", provider.label());
                        }
                    }
                }
                if ui
                    .add_enabled(
                        self.background_task.is_none(),
                        egui::Button::new("Test cloud API"),
                    )
                    .on_hover_text("Checks the selected provider key and model from Rust without starting the Python worker.")
                    .clicked()
                {
                    self.run_cloud_api_check();
                }
                ui.label(&self.stt_api_key_status);
            });
            ui.label(
                "Paste or edit the API key above, then click Save settings. Clear the field and save to remove the stored key.",
            );
        }
        if self.settings.stt_backend == "openai" {
            ui.label(
                "Cloud STT sends recorded audio to the configured provider. API keys are stored in the OS credential store when saved from this UI.",
            );
        }
    }

    fn quality_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Quality");
        egui::Grid::new("quality_settings")
            .num_columns(2)
            .show(ui, |ui| {
                text_help(
                    ui,
                    "Beam size",
                    &mut self.settings.beam_size,
                    "Whisper beam search width. Higher can improve accuracy but costs more compute.",
                );
                text_help(
                    ui,
                    "Temperature ladder",
                    &mut self.settings.temperature,
                    "Comma-separated Whisper fallback temperatures, for example 0.0,0.2.",
                );
                text_help(
                    ui,
                    "Context min seconds",
                    &mut self.settings.context_min_seconds,
                    "Minimum utterance length before passing previous context/prompt hints to Whisper.",
                );
                text_help(
                    ui,
                    "Parakeet min seconds",
                    &mut self.settings.parakeet_min_seconds,
                    "Minimum captured audio length before Parakeet transcription is attempted.",
                );
                text_help(
                    ui,
                    "Release tail ms",
                    &mut self.settings.release_tail_ms,
                    "Extra audio kept after releasing the hotkey so word endings are not clipped.",
                );
                text_help(
                    ui,
                    "VAD threshold",
                    &mut self.settings.vad_threshold,
                    "Voice activity detection sensitivity. Lower is more sensitive, higher rejects more noise.",
                );
                text_help(
                    ui,
                    "VAD min silence ms",
                    &mut self.settings.vad_min_silence_ms,
                    "Silence duration used by VAD to split or end speech.",
                );
                text_help(
                    ui,
                    "Target dBFS",
                    &mut self.settings.target_dbfs,
                    "Audio normalization target loudness before transcription.",
                );
                text_help(
                    ui,
                    "Min input dBFS",
                    &mut self.settings.min_input_dbfs,
                    "Minimum raw microphone loudness accepted as speech candidate.",
                );
                text_help(
                    ui,
                    "Min SNR dB",
                    &mut self.settings.min_snr_db,
                    "Minimum signal-to-noise ratio accepted before transcription.",
                );
                checkbox_help(
                    ui,
                    "Audio ducking",
                    &mut self.settings.audio_ducking,
                    "Windows-only: temporarily lowers other app audio while recording, then restores it.",
                );
                text_help(
                    ui,
                    "Audio ducking level",
                    &mut self.settings.audio_ducking_level,
                    "Target volume for other apps while recording. 0.25 means 25%.",
                );
            });
        let show_initial_prompt_help = label_with_help(
            ui,
            "Initial prompt",
            "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately.",
        );
        inline_help(ui, show_initial_prompt_help, "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately.");
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.initial_prompt)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
    }

    fn dictionary_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Dictionary");
        ui.horizontal(|ui| {
            if ui.button("Ensure file").clicked() {
                self.ensure_dictionary();
            }
            if ui.button("Open").clicked() {
                self.open_dictionary();
            }
            if ui.button("Preview").clicked() {
                self.preview_dictionary();
            }
        });
        egui::Grid::new("dictionary_settings")
            .num_columns(2)
            .show(ui, |ui| {
                text_help(
                    ui,
                    "Dictionary path",
                    &mut self.settings.dictionary,
                    "JSON dictionary used for prompt terms and deterministic replacements.",
                );
                checkbox_help(
                    ui,
                    "Dictionary enabled",
                    &mut self.settings.dictionary_enabled,
                    "Enable prompt-term injection and replacement cleanup from the dictionary.",
                );
                text_help(
                    ui,
                    "Max prompt terms",
                    &mut self.settings.dictionary_max_terms,
                    "Maximum number of dictionary terms included in the model prompt.",
                );
                text_help(
                    ui,
                    "Prompt char cap",
                    &mut self.settings.dictionary_prompt_chars,
                    "Maximum characters used by dictionary prompt terms to avoid over-steering the model.",
                );
            });
        if !self.dictionary_preview.is_empty() {
            ui.label("Prompt preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.dictionary_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
    }

    fn output_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Output");
        egui::Grid::new("output_settings")
            .num_columns(2)
            .show(ui, |ui| {
                combo_help(
                    ui,
                    "Inject mode",
                    &mut self.settings.inject_mode,
                    &["auto", "type", "paste", "print"],
                    "How text is inserted into the focused app. auto chooses the safest available strategy.",
                );
                combo_help(
                    ui,
                    "Format commands",
                    &mut self.settings.format_commands,
                    &["off", "en", "da", "both"],
                    "Enable spoken formatting commands such as punctuation and new lines.",
                );
                checkbox_help(
                    ui,
                    "JSON stdout",
                    &mut self.settings.inject_json,
                    "Emit structured JSON events to stdout in addition to normal logs.",
                );
                text_help(
                    ui,
                    "Metrics JSONL",
                    &mut self.settings.metrics_jsonl,
                    "Optional path for appending transcription metrics as JSONL.",
                );
                text_help(
                    ui,
                    "Command hook",
                    &mut self.settings.command_hook,
                    "Optional command run after accepted utterances for advanced automation.",
                );
                text_help(
                    ui,
                    "Command hook timeout ms",
                    &mut self.settings.command_hook_timeout_ms,
                    "Maximum time the command hook may run before it is treated as timed out.",
                );
                combo_help(
                    ui,
                    "Post processor",
                    &mut self.settings.post_processor,
                    &["none", "ollama", "openai", "groq"],
                    "Optional second pass that rewrites text after transcription. Groq/OpenAI use cloud chat models; Ollama stays local.",
                );
                combo_help(
                    ui,
                    "Post mode",
                    &mut self.settings.post_mode,
                    &[
                        "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
                    ],
                    "Output style used by the post-processor.",
                );
                match self.settings.post_processor.as_str() {
                    "groq" => combo_help(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        GROQ_POST_MODELS,
                        "Groq chat model used for the optional final text cleanup pass. STT Whisper models are not listed here because they transcribe audio, not text.",
                    ),
                    "openai" => combo_help(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        OPENAI_POST_MODELS,
                        "OpenAI chat model used for the optional final text cleanup pass.",
                    ),
                    _ => text_help(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        "Model name for post-processing, for example an Ollama model.",
                    ),
                }
                text_help(
                    ui,
                    "Post base URL",
                    &mut self.settings.post_base_url,
                    "Base URL for the post-processing provider.",
                );
                text_help(
                    ui,
                    "Post timeout ms",
                    &mut self.settings.post_timeout_ms,
                    "Maximum time allowed for post-processing.",
                );
                text_help(
                    ui,
                    "Post max input chars",
                    &mut self.settings.post_max_input_chars,
                    "Maximum transcript length sent to the post-processor.",
                );
                text_help(
                    ui,
                    "Post max output chars",
                    &mut self.settings.post_max_output_chars,
                    "Maximum accepted length of post-processed output.",
                );
                checkbox_help(
                    ui,
                    "Cloud redaction",
                    &mut self.settings.post_redact,
                    "Before OpenAI-compatible post-processing, replace sensitive local text with placeholders and restore it afterward when possible.",
                );
                text_help(
                    ui,
                    "Redaction terms",
                    &mut self.settings.post_redact_terms,
                    "Comma-separated names or terms to redact before cloud post-processing. Emails, phone numbers and common tokens are detected automatically.",
                );
                checkbox_help(
                    ui,
                    "History enabled",
                    &mut self.settings.history_enabled,
                    "Store local utterance history for review, copying and dictionary suggestions.",
                );
                text_help(
                    ui,
                    "History JSONL",
                    &mut self.settings.history_jsonl,
                    "Optional override path for local utterance history JSONL.",
                );
                checkbox_help(
                    ui,
                    "Local only",
                    &mut self.settings.local_only,
                    "Block network-backed STT/post-processing providers when enabled.",
                );
                checkbox_help(
                    ui,
                    "VOICEPI_DEBUG",
                    &mut self.settings.debug,
                    "Print the effective configuration at worker startup.",
                );
                checkbox_help(
                    ui,
                    "VOICEPI_STT_DEBUG",
                    &mut self.settings.stt_debug,
                    "Enable extra backend transcription diagnostics.",
                );
                text_help(
                    ui,
                    "UI text scale",
                    &mut self.settings.ui_text_scale,
                    "Scale all text in this settings UI. Use 1.0 for default, 1.15 for larger text, or 1.3 for high-DPI displays.",
                );
            });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Preview history").clicked() {
                self.preview_history();
            }
            if ui.button("Open history").clicked() {
                self.open_history();
            }
            if ui.button("Preview metrics").clicked() {
                self.preview_metrics();
            }
            if ui.button("Open metrics").clicked() {
                self.open_metrics();
            }
        });
        if !self.history_preview.is_empty() {
            ui.label("History preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.history_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
        if !self.metrics_preview.is_empty() {
            ui.label("Metrics preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.metrics_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
    }

    fn profiles_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        let show_profiles_help = label_with_help(
            ui,
            "Profiles JSON",
            "Advanced JSON profile definitions. Save persists valid JSON profiles into the config file.",
        );
        inline_help(
            ui,
            show_profiles_help,
            "Advanced JSON profile definitions. Save persists valid JSON profiles into the config file.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.profiles_json)
                .font(egui::TextStyle::Monospace)
                .desired_rows(22)
                .desired_width(f32::INFINITY),
        );
    }

    fn start_runtime(&mut self) {
        let command = self.worker_command();
        self.append_runtime_log(format!("[ui] starting: {}", command.display()));
        if let Err(err) = self.supervisor.start(command) {
            self.append_runtime_log(format!("[ui] start failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    fn stop_runtime(&mut self) {
        self.append_runtime_log("[ui] stopping runtime");
        if let Err(err) = self.supervisor.stop() {
            self.append_runtime_log(format!("[ui] stop failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    fn restart_runtime(&mut self) {
        let command = self.worker_command();
        self.append_runtime_log(format!("[ui] restarting: {}", command.display()));
        if let Err(err) = self.supervisor.restart(command) {
            self.append_runtime_log(format!("[ui] restart failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    fn worker_command(&self) -> WorkerCommand {
        let mut command = default_worker_command();
        if self.settings.stt_backend == "openai" {
            let key = self.stt_api_key_input.trim();
            if !key.is_empty() {
                command
                    .env
                    .push((STT_API_KEY_ENV.to_owned(), key.to_owned()));
            }
        }
        if matches!(self.settings.post_processor.as_str(), "openai" | "groq") {
            let key = self.stt_api_key_input.trim();
            if !key.is_empty() {
                command
                    .env
                    .push((POST_API_KEY_ENV.to_owned(), key.to_owned()));
            }
        }
        command
    }

    fn run_doctor(&mut self) {
        let command = doctor_command();
        self.append_runtime_log(format!("[ui] doctor: {}", command.display()));
        match run_capture(&command) {
            Ok(output) => {
                self.append_runtime_output(output.stdout.trim_end());
                self.append_runtime_output(output.stderr.trim_end());
                if output.success() {
                    self.append_runtime_log("[ui] doctor passed");
                } else {
                    self.append_runtime_log(format!(
                        "[ui] doctor failed with code {}",
                        output
                            .code()
                            .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
                    ));
                }
            }
            Err(err) => self.append_runtime_log(format!("[ui] doctor failed to run: {err}")),
        }
    }

    fn run_install(&mut self) {
        self.run_background_command("install/repair", install_command());
    }

    fn run_cloud_api_check(&mut self) {
        if self.background_task.is_some() {
            self.append_runtime_log("[ui] cloud API check skipped: another task is running");
            return;
        }

        let check = match CloudApiCheck::from_settings(&self.settings, &self.stt_api_key_input) {
            Ok(check) => check,
            Err(err) => {
                self.stt_api_key_status = format!("Cloud API check failed: {err}");
                self.append_runtime_log(format!("[ui] cloud API check failed: {err}"));
                return;
            }
        };
        self.append_runtime_log(format!(
            "[ui] cloud API check: {} {}",
            check.provider, check.model
        ));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = match check_cloud_api(&check) {
                Ok(result) => BackgroundTaskResult {
                    label: "cloud API check",
                    command: format!("{} /models", check.provider),
                    stdout: result.summary(),
                    stderr: String::new(),
                    success: result.model_available,
                    code: None,
                    error: None,
                },
                Err(err) => BackgroundTaskResult {
                    label: "cloud API check",
                    command: format!("{} /models", check.provider),
                    stdout: String::new(),
                    stderr: String::new(),
                    success: false,
                    code: None,
                    error: Some(err.to_string()),
                },
            };
            let _ = tx.send(result);
        });
        self.background_task = Some(rx);
        self.background_task_label = Some("cloud API check");
    }

    fn run_background_command(&mut self, label: &'static str, command: WorkerCommand) {
        if self.background_task.is_some() {
            self.append_runtime_log(format!("[ui] {label} skipped: another task is running"));
            return;
        }

        let display = command.display();
        self.append_runtime_log(format!("[ui] {label}: {display}"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = match run_capture(&command) {
                Ok(output) => {
                    let success = output.success();
                    let code = output.code();
                    BackgroundTaskResult {
                        label,
                        command: display,
                        stdout: output.stdout,
                        stderr: output.stderr,
                        success,
                        code,
                        error: None,
                    }
                }
                Err(err) => BackgroundTaskResult {
                    label,
                    command: display,
                    stdout: String::new(),
                    stderr: String::new(),
                    success: false,
                    code: None,
                    error: Some(err.to_string()),
                },
            };
            let _ = tx.send(result);
        });
        self.background_task = Some(rx);
        self.background_task_label = Some(label);
    }

    fn poll_background_task(&mut self) {
        let Some(rx) = self.background_task.as_ref() else {
            return;
        };

        let result = match rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(BackgroundTaskResult {
                label: self.background_task_label.unwrap_or("background task"),
                command: String::new(),
                stdout: String::new(),
                stderr: String::new(),
                success: false,
                code: None,
                error: Some("background task stopped without reporting a result".to_owned()),
            }),
        };

        if let Some(result) = result {
            self.background_task = None;
            self.background_task_label = None;
            self.append_runtime_log(format!(
                "[ui] {} completed: {}",
                result.label, result.command
            ));
            self.append_runtime_output(result.stdout.trim_end());
            self.append_runtime_output(result.stderr.trim_end());
            if let Some(error) = result.error {
                self.append_runtime_log(format!("[ui] {} failed to run: {error}", result.label));
            } else if result.success {
                self.append_runtime_log(format!("[ui] {} passed", result.label));
            } else {
                self.append_runtime_log(format!(
                    "[ui] {} failed with code {}",
                    result.label,
                    result
                        .code
                        .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
                ));
            }
        }
    }

    fn poll_runtime(&mut self) {
        for event in self.supervisor.poll() {
            match event {
                RuntimeEvent::Started { command } => {
                    self.append_runtime_log(format!("[ui] started: {command}"));
                }
                RuntimeEvent::Worker(event) => {
                    if event.event == "status" {
                        if let Some(state) = event.state {
                            self.append_runtime_log(format!("[worker] status={state}"));
                        }
                    }
                }
                RuntimeEvent::Stdout(line) | RuntimeEvent::Stderr(line) => {
                    self.append_runtime_log(line);
                }
                RuntimeEvent::Exited { code } => {
                    self.append_runtime_log(format!(
                        "[ui] runtime exited with code {}",
                        code.map_or_else(|| "unknown".to_owned(), |c| c.to_string())
                    ));
                }
                RuntimeEvent::Error(message) => {
                    self.append_runtime_log(format!("[ui] runtime error: {message}"));
                }
            }
        }
        self.runtime_state = self.supervisor.state();
    }

    fn append_runtime_log(&mut self, line: impl AsRef<str>) {
        if !self.runtime_log.is_empty() {
            self.runtime_log.push('\n');
        }
        self.runtime_log.push_str(line.as_ref());
    }

    fn append_runtime_output(&mut self, output: &str) {
        if output.is_empty() {
            return;
        }
        self.append_runtime_log(output);
    }

    fn save_settings(&mut self) {
        self.normalize_cloud_provider_settings();
        self.normalize_postprocessor_settings();
        if let Err(err) = serde_json::from_str::<serde_json::Value>(&self.settings.profiles_json) {
            self.settings_status = format!("Profiles JSON is invalid: {err}");
            return;
        }
        match config::save_settings(&self.settings) {
            Ok(path) => {
                let restart_keys =
                    config::restart_required_keys(&self.saved_settings, &self.settings);
                let key_message = self.save_stt_api_key_if_changed();
                self.saved_settings = self.settings.clone();
                self.settings_status = format!("Saved settings: {}", path.display());
                if let Some(message) = key_message {
                    self.settings_status.push_str(" | ");
                    self.settings_status.push_str(&message);
                }
                if self.supervisor.is_running() && !restart_keys.is_empty() {
                    self.append_runtime_log(format!(
                        "[ui] restart required after settings change: {}",
                        restart_keys.join(", ")
                    ));
                    self.restart_runtime();
                }
            }
            Err(err) => {
                self.settings_status = format!("Save failed: {err}");
            }
        }
    }

    fn has_unsaved_settings(&self) -> bool {
        self.settings != self.saved_settings
            || self.stt_api_key_input != self.saved_stt_api_key_input
    }

    fn reload_settings(&mut self) {
        match config::load_settings() {
            Ok(settings) => {
                self.saved_settings = settings.clone();
                self.settings = settings;
                self.reload_stt_api_key();
                self.settings_status = "Reloaded config".to_owned();
            }
            Err(err) => {
                self.settings_status = format!("Reload failed: {err}");
            }
        }
    }

    fn current_cloud_provider(&self) -> CloudProvider {
        CloudProvider::from_raw(&self.settings.stt_provider)
            .unwrap_or_else(|| CloudProvider::from_settings(&self.settings))
    }

    fn set_cloud_provider(&mut self, provider: CloudProvider) {
        self.settings.stt_backend = "openai".to_owned();
        self.apply_cloud_provider_defaults(provider);
        self.reload_stt_api_key();
    }

    fn normalize_cloud_provider_settings(&mut self) {
        if self.settings.stt_backend == "openai" {
            let provider = self.current_cloud_provider();
            self.apply_cloud_provider_defaults(provider);
        }
    }

    fn apply_cloud_provider_defaults(&mut self, provider: CloudProvider) {
        self.settings.stt_provider = provider.id().to_owned();
        self.settings.stt_base_url = provider.base_url().to_owned();
        if !provider
            .model_options()
            .contains(&self.settings.stt_model.as_str())
        {
            self.settings.stt_model = provider.default_model().to_owned();
        }
    }

    fn normalize_postprocessor_settings(&mut self) {
        match self.settings.post_processor.as_str() {
            "groq" => {
                self.settings.post_base_url = GROQ_STT_BASE_URL.to_owned();
                if !GROQ_POST_MODELS.contains(&self.settings.post_model.as_str()) {
                    self.settings.post_model = GROQ_POST_MODEL.to_owned();
                }
            }
            "openai" => {
                self.settings.post_base_url = OPENAI_STT_BASE_URL.to_owned();
                if !OPENAI_POST_MODELS.contains(&self.settings.post_model.as_str()) {
                    self.settings.post_model = OPENAI_POST_MODEL.to_owned();
                }
            }
            "ollama" => {
                if self.settings.post_base_url.trim().is_empty()
                    || self.settings.post_base_url == GROQ_STT_BASE_URL
                    || self.settings.post_base_url == OPENAI_STT_BASE_URL
                {
                    self.settings.post_base_url = "http://localhost:11434".to_owned();
                }
            }
            _ => {}
        }
    }

    fn reload_stt_api_key(&mut self) {
        let provider = self.current_cloud_provider();
        match load_stt_api_key_state(provider) {
            Ok((key, saved_key, status)) => {
                self.stt_api_key_input = key;
                self.saved_stt_api_key_input = saved_key;
                self.stt_api_key_status = status;
            }
            Err(err) => {
                self.stt_api_key_input.clear();
                self.saved_stt_api_key_input.clear();
                self.stt_api_key_status = format!("Could not load API key: {err}");
            }
        }
    }

    fn save_stt_api_key_if_changed(&mut self) -> Option<String> {
        if self.settings.stt_backend != "openai" {
            return None;
        }
        if self.stt_api_key_input == self.saved_stt_api_key_input {
            return None;
        }
        let provider = self.current_cloud_provider();
        let message = match save_stt_api_key(provider, self.stt_api_key_input.trim()) {
            Ok(()) => {
                self.saved_stt_api_key_input = self.stt_api_key_input.clone();
                if self.stt_api_key_input.trim().is_empty() {
                    format!("Cleared saved {} API key.", provider.label())
                } else {
                    format!("Saved {} API key in OS credential store.", provider.label())
                }
            }
            Err(err) => {
                format!("Could not save {} API key: {err}", provider.label())
            }
        };
        self.stt_api_key_status = message.clone();
        Some(message)
    }

    fn ensure_dictionary(&mut self) {
        match config::ensure_dictionary_file(&self.settings.dictionary) {
            Ok(path) => {
                self.settings_status = format!("Dictionary ready: {}", path.display());
            }
            Err(err) => {
                self.settings_status = format!("Dictionary create failed: {err}");
            }
        }
    }

    fn open_dictionary(&mut self) {
        match config::open_dictionary(&self.settings.dictionary) {
            Ok(path) => {
                self.settings_status = format!("Opened dictionary: {}", path.display());
            }
            Err(err) => {
                self.settings_status = format!("Open dictionary failed: {err}");
            }
        }
    }

    fn history_path(&self) -> std::path::PathBuf {
        if self.settings.history_jsonl.trim().is_empty() {
            config::default_history_path()
        } else {
            std::path::PathBuf::from(self.settings.history_jsonl.trim())
        }
    }

    fn preview_history(&mut self) {
        let path = self.history_path();
        match telemetry::preview_jsonl(&path, 20) {
            Ok(preview) => {
                self.history_preview = format!(
                    "{}\nrows: showing {} of {}\n{}",
                    preview.path.display(),
                    preview.shown_rows,
                    preview.total_rows,
                    preview.text
                );
                self.settings_status = format!("Loaded history preview: {}", path.display());
            }
            Err(err) => {
                self.history_preview.clear();
                self.settings_status = format!("History preview failed: {err}");
            }
        }
    }

    fn open_history(&mut self) {
        let path = self.history_path();
        match config::open_existing_path(&path) {
            Ok(path) => self.settings_status = format!("Opened history: {}", path.display()),
            Err(err) => self.settings_status = format!("Open history failed: {err}"),
        }
    }

    fn preview_metrics(&mut self) {
        let raw = self.settings.metrics_jsonl.trim();
        if raw.is_empty() {
            self.metrics_preview.clear();
            self.settings_status = "Metrics JSONL path is unset.".to_owned();
            return;
        }
        let path = std::path::PathBuf::from(raw);
        match telemetry::preview_jsonl(&path, 20) {
            Ok(preview) => {
                self.metrics_preview = format!(
                    "{}\nrows: showing {} of {}\n{}",
                    preview.path.display(),
                    preview.shown_rows,
                    preview.total_rows,
                    preview.text
                );
                self.settings_status = format!("Loaded metrics preview: {}", path.display());
            }
            Err(err) => {
                self.metrics_preview.clear();
                self.settings_status = format!("Metrics preview failed: {err}");
            }
        }
    }

    fn open_metrics(&mut self) {
        let raw = self.settings.metrics_jsonl.trim();
        if raw.is_empty() {
            self.settings_status = "Metrics JSONL path is unset.".to_owned();
            return;
        }
        match config::open_existing_path(raw) {
            Ok(path) => self.settings_status = format!("Opened metrics: {}", path.display()),
            Err(err) => self.settings_status = format!("Open metrics failed: {err}"),
        }
    }

    fn preview_dictionary(&mut self) {
        let max_terms = self
            .settings
            .dictionary_max_terms
            .parse::<usize>()
            .unwrap_or(80);
        let max_chars = self
            .settings
            .dictionary_prompt_chars
            .parse::<usize>()
            .unwrap_or(1200);
        match dictionary::preview_dictionary(
            self.settings.dictionary.clone(),
            Some(&self.settings.initial_prompt),
            max_terms,
            max_chars,
        ) {
            Ok(preview) => {
                self.settings_status = format!(
                    "Dictionary preview: {} terms, {} replacements",
                    preview.term_count, preview.replacement_count
                );
                self.dictionary_preview = preview.prompt.unwrap_or_else(|| {
                    "(No prompt terms selected by current dictionary limits)".to_owned()
                });
            }
            Err(err) => {
                self.settings_status = format!("Dictionary preview failed: {err}");
                self.dictionary_preview.clear();
            }
        }
    }
}

fn text_help(ui: &mut egui::Ui, label: &str, value: &mut String, help: &str) {
    let show_help = label_with_help(ui, label, help);
    ui.add(egui::TextEdit::singleline(value).desired_width(360.0));
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn text_enabled(ui: &mut egui::Ui, enabled: bool, label: &str, value: &mut String, help: &str) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    ui.add_enabled_ui(enabled, |ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(360.0));
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn password_enabled(ui: &mut egui::Ui, enabled: bool, label: &str, value: &mut String, help: &str) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    ui.add_enabled_ui(enabled, |ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .password(true)
                .desired_width(360.0),
        );
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn checkbox_help(ui: &mut egui::Ui, label: &str, value: &mut bool, help: &str) {
    let show_help = label_with_help(ui, label, help);
    ui.checkbox(value, "");
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn combo_help(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str], help: &str) {
    let show_help = label_with_help(ui, label, help);
    egui::ComboBox::from_id_salt(label)
        .selected_text(if value.is_empty() {
            "(empty)"
        } else {
            value.as_str()
        })
        .show_ui(ui, |ui| {
            for option in options {
                ui.selectable_value(
                    value,
                    (*option).to_owned(),
                    if option.is_empty() { "(empty)" } else { option },
                );
            }
        });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn combo_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[&str],
    help: &str,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(label)
            .selected_text(if value.is_empty() {
                "(empty)"
            } else {
                value.as_str()
            })
            .show_ui(ui, |ui| {
                for option in options {
                    ui.selectable_value(
                        value,
                        (*option).to_owned(),
                        if option.is_empty() { "(empty)" } else { option },
                    );
                }
            });
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn label_with_help(ui: &mut egui::Ui, label: &str, help: &str) -> bool {
    ui.horizontal(|ui| {
        let response = ui.label(label);
        if !help.is_empty() {
            response.on_hover_text(help);
        }
        help_badge(ui, label, help)
    })
    .inner
}

fn label_with_help_enabled(ui: &mut egui::Ui, enabled: bool, label: &str, help: &str) -> bool {
    ui.horizontal(|ui| {
        let response = ui.add_enabled(enabled, egui::Label::new(label));
        if !help.is_empty() {
            response.on_hover_text(help);
        }
        help_badge(ui, label, help)
    })
    .inner
}

fn help_badge(ui: &mut egui::Ui, label: &str, help: &str) -> bool {
    if help.is_empty() {
        return false;
    }

    let id = ui.make_persistent_id(("settings_help", label));
    let mut show_help = ui
        .data_mut(|data| data.get_persisted::<bool>(id))
        .unwrap_or(false);
    let response = ui.small_button("?");
    if response.clicked() {
        show_help = !show_help;
        ui.data_mut(|data| data.insert_persisted(id, show_help));
    }
    let _ = response.on_hover_text(help);
    show_help
}

fn grid_help_row(ui: &mut egui::Ui, show_help: bool, help: &str) {
    if show_help {
        ui.label("");
        inline_help(ui, true, help);
        ui.end_row();
    }
}

fn inline_help(ui: &mut egui::Ui, show_help: bool, help: &str) {
    if show_help {
        ui.label(egui::RichText::new(help).color(ui.visuals().weak_text_color()));
    }
}

fn apply_ui_text_scale(ctx: &egui::Context, raw_scale: &str) {
    let scale = raw_scale
        .trim()
        .parse::<f32>()
        .unwrap_or(DEFAULT_UI_TEXT_SCALE)
        .clamp(0.85, 1.6);
    let text_styles = BTreeMap::from([
        (
            egui::TextStyle::Heading,
            egui::FontId::proportional(18.0 * scale),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::proportional(14.0 * scale),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::monospace(13.0 * scale),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::proportional(14.0 * scale),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::proportional(12.0 * scale),
        ),
    ]);
    let mut style = (*ctx.style()).clone();
    if style.text_styles != text_styles {
        style.text_styles = text_styles;
        ctx.set_style(style);
    }
}

fn app_icon() -> egui::IconData {
    const SIZE: u32 = 256;
    let mut rgba = vec![0; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let idx = ((y * SIZE + x) * 4) as usize;
            if !inside_rounded_square(x as i32, y as i32, SIZE as i32, 60) {
                rgba[idx + 3] = 0;
                continue;
            }
            let t = (x + y) as f32 / ((SIZE - 1) * 2) as f32;
            rgba[idx] = 255;
            rgba[idx + 1] = (42.0 * (1.0 - t)) as u8;
            rgba[idx + 2] = (42.0 * (1.0 - t) + 16.0 * t) as u8;
            rgba[idx + 3] = 255;
        }
    }

    for (x, y, w, h, r) in [
        (56, 112, 16, 32, 8),
        (84, 92, 16, 72, 8),
        (112, 64, 16, 128, 8),
        (140, 84, 16, 88, 8),
        (168, 104, 16, 48, 8),
        (196, 118, 16, 20, 8),
    ] {
        fill_rounded_rect(&mut rgba, SIZE, x, y, w, h, r, [255, 255, 255, 255]);
    }

    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

fn inside_rounded_square(x: i32, y: i32, size: i32, radius: i32) -> bool {
    inside_rounded_rect(x, y, 0, 0, size, size, radius)
}

fn fill_rounded_rect(
    rgba: &mut [u8],
    canvas: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    color: [u8; 4],
) {
    for py in y..(y + height) {
        for px in x..(x + width) {
            if inside_rounded_rect(px, py, x, y, width, height, radius) {
                let idx = ((py as u32 * canvas + px as u32) * 4) as usize;
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
}

fn inside_rounded_rect(
    px: i32,
    py: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
) -> bool {
    if width <= 0 || height <= 0 {
        return false;
    }
    let radius = radius.max(0).min((width - 1) / 2).min((height - 1) / 2);
    let left = x + radius;
    let right = x + width - radius - 1;
    let top = y + radius;
    let bottom = y + height - radius - 1;
    let cx = px.clamp(left, right);
    let cy = py.clamp(top, bottom);
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= radius * radius
}

fn open_url(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn()?;
        Ok(())
    }
}

fn load_stt_api_key(provider: CloudProvider) -> Result<String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, provider.credential_user())?;
    match entry.get_password() {
        Ok(secret) => Ok(secret),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(err) => Err(err.into()),
    }
}

fn load_stt_api_key_state(provider: CloudProvider) -> Result<(String, String, String)> {
    let key = load_stt_api_key(provider)?;
    if !key.is_empty() {
        let status = format!(
            "Loaded saved {} API key from credential store.",
            provider.label()
        );
        return Ok((key.clone(), key, status));
    }

    if let Some(env_key) = load_stt_api_key_from_env(provider) {
        let status = format!(
            "Loaded {} API key from environment. Save settings to store it.",
            provider.label()
        );
        return Ok((env_key, String::new(), status));
    }

    Ok((
        String::new(),
        String::new(),
        format!("No {} API key saved.", provider.label()),
    ))
}

fn load_stt_api_key_from_env(provider: CloudProvider) -> Option<String> {
    let candidates: &[&str] = match provider {
        CloudProvider::Groq => &["VOICEPI_STT_API_KEY", "GROQ_API_KEY"],
        CloudProvider::OpenAi => &["VOICEPI_STT_API_KEY", "OPENAI_API_KEY"],
    };
    candidates
        .iter()
        .filter_map(|name| env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
}

fn save_stt_api_key(provider: CloudProvider, secret: &str) -> Result<()> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, provider.credential_user())?;
    if secret.trim().is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    } else {
        entry.set_password(secret.trim())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn app_icon_builds_valid_rgba_buffer() {
        let icon = app_icon();

        assert_eq!(icon.width, 256);
        assert_eq!(icon.height, 256);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        assert!(icon
            .rgba
            .chunks_exact(4)
            .any(|px| px == [255, 255, 255, 255]));
    }

    #[test]
    fn rounded_rect_handles_radius_larger_than_half_width() {
        assert!(inside_rounded_rect(15, 72, 8, 56, 16, 32, 8));
    }

    #[test]
    fn rounded_rect_rejects_empty_dimensions_without_panicking() {
        assert!(!inside_rounded_rect(0, 0, 0, 0, 0, 16, 8));
        assert!(!inside_rounded_rect(0, 0, 0, 0, 16, 0, 8));
    }

    #[test]
    fn stt_backend_mode_maps_only_active_backend() {
        assert_eq!(SttBackendMode::from_raw("whisper"), SttBackendMode::Whisper);
        assert_eq!(
            SttBackendMode::from_raw("parakeet"),
            SttBackendMode::Parakeet
        );
        assert_eq!(SttBackendMode::from_raw("openai"), SttBackendMode::Cloud);
        assert_eq!(SttBackendMode::from_raw(""), SttBackendMode::Whisper);
    }

    #[test]
    fn provider_api_key_can_load_from_environment_fallback() {
        let _stt = EnvVarGuard::remove("VOICEPI_STT_API_KEY");
        let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
        let _groq = EnvVarGuard::set("GROQ_API_KEY", "groq-test-key");

        assert_eq!(
            load_stt_api_key_from_env(CloudProvider::Groq).as_deref(),
            Some("groq-test-key")
        );
        assert_eq!(load_stt_api_key_from_env(CloudProvider::OpenAi), None);

        unsafe {
            env::set_var("VOICEPI_STT_API_KEY", "shared-test-key");
        }

        assert_eq!(
            load_stt_api_key_from_env(CloudProvider::Groq).as_deref(),
            Some("shared-test-key")
        );
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = env::var_os(key);
            unsafe {
                env::remove_var(key);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.original {
                    Some(value) => env::set_var(self.key, value),
                    None => env::remove_var(self.key),
                }
            }
        }
    }
}
