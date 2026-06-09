//! Desktop UI module root. This file is deliberately thin: it wires up the UI
//! submodules, re-exports their shared items into `crate::ui` so the settings
//! tabs (which import `super::super::*`) keep resolving, and owns the small
//! cross-cutting definitions — the model option tables, the `WhisperDictateApp`
//! state struct, its `Default`, the `Tab` enum, and the `eframe` `run` entry.
//!
//! Behaviour lives in the submodules:
//! - `app`            — the `eframe::App` `update` loop + runtime lifecycle/polling
//! - `tasks`          — background doctor/install runs and API connectivity checks
//! - `settings_state` — config save/reload + provider/API-key persistence
//! - `previews`       — dictionary/history/metrics file helpers and previews
//! - `theme`          — palette, colour/dimension constants, egui style + chrome
//! - `text`           — localized UI strings (`UiTextKey`/`ui_text`)
//! - `widgets`        — reusable settings-grid form rows and help badges
//! - `log_render`     — runtime-log view modes and card parsing
//! - `worker_event`   — worker JSON event parsing + audio-meter helpers
//! - `platform`       — STT backend mode, XKB layout detection, `open_url`
//! - `api_keys`       — secret storage / cloud + post provider model
//! - `tabs`           — per-tab rendering (`impl WhisperDictateApp` UI methods)

use anyhow::Result;
use eframe::egui;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::config::{self, AppSettings};
use crate::runtime::{self, RuntimeState, RuntimeSupervisor};
// Re-exported only for the headless `*_tests.rs` modules that build worker events
// via `super::*`; non-test code imports `WorkerEvent` from `crate::runtime`.
#[cfg(test)]
pub(in crate::ui) use crate::runtime::WorkerEvent;

mod api_keys;
mod app;
mod icon;
mod log_render;
mod platform;
mod previews;
mod secret_store;
mod settings_state;
mod tabs;
mod tasks;
mod text;
mod theme;
mod widgets;
mod worker_event;

use self::api_keys::*;
use self::icon::app_icon;
// Re-exported so the secret-store `*_tests.rs` modules (which import `super::*`)
// resolve these items; non-test code reaches them through `api_keys`.
pub(in crate::ui) use self::log_render::*;
pub(in crate::ui) use self::platform::*;
#[cfg(test)]
use self::secret_store::*;
pub(in crate::ui) use self::text::*;
pub(in crate::ui) use self::theme::*;
pub(in crate::ui) use self::widgets::*;
pub(in crate::ui) use self::worker_event::*;

// Ordered most → least accurate. Larger models are more accurate but slower
// and need more VRAM; see `whisper_model_hint` for per-model annotations.
const WHISPER_MODELS: &[&str] = &[
    "large-v3",
    "large-v3-turbo",
    "medium",
    "small",
    "base",
    "tiny",
];

/// Accuracy/speed note + approximate VRAM (MB, at the GPU `int8_float16`
/// default) for a Whisper model value. Drives the model picker's labels and
/// the "does it fit my GPU" grey-out.
fn whisper_model_hint(model: &str) -> (&'static str, u32) {
    match model {
        "large-v3" => ("most accurate, slowest", 3200),
        "large-v3-turbo" => ("great accuracy, fast", 1800),
        "medium" => ("good accuracy, lighter", 1500),
        "small" => ("ok accuracy, fast & light", 1000),
        "base" => ("low accuracy, very light", 600),
        "tiny" => ("lowest accuracy, fastest", 400),
        _ => ("", 0),
    }
}

/// Best total VRAM (MB) across detected NVIDIA GPUs, or `None` on CPU /
/// non-NVIDIA. Detected once at startup and used to grey out Whisper models
/// that can't fit the GPU.
fn best_gpu_total_mb() -> Option<u32> {
    crate::model_capacity::query_gpus()
        .iter()
        .map(|gpu| gpu.total_mb)
        .max()
}
const GROQ_STT_MODELS: &[&str] = &[
    "whisper-large-v3-turbo",
    "whisper-large-v3",
    "distil-whisper-large-v3-en",
];
const OPENAI_STT_MODELS: &[&str] = &["gpt-4o-mini-transcribe", "gpt-4o-transcribe", "whisper-1"];
const STT_BACKEND_OPTIONS: &[(&str, &str)] = &[
    ("whisper", "Local Whisper"),
    ("parakeet", "Local NVIDIA Parakeet"),
    ("openai", "Cloud STT (Groq/OpenAI)"),
];
const CLOUD_PROVIDER_OPTIONS: &[(&str, &str)] = &[("groq", "Groq"), ("openai", "OpenAI")];
const POST_PROCESSOR_OPTIONS: &[(&str, &str)] = &[
    ("none", "Disabled"),
    ("ollama", "Local Ollama"),
    ("openai", "OpenAI"),
    ("groq", "Groq"),
];
const GROQ_POST_MODELS: &[(&str, &str)] = &[
    (
        "llama-3.3-70b-versatile",
        "llama-3.3-70b-versatile - recommended Danish final check",
    ),
    (
        "qwen/qwen3-32b",
        "qwen/qwen3-32b - strong multilingual, use hidden reasoning",
    ),
    (
        "openai/gpt-oss-20b",
        "openai/gpt-oss-20b - fast quality/cost candidate",
    ),
    (
        "openai/gpt-oss-120b",
        "openai/gpt-oss-120b - highest quality, heavier",
    ),
    (
        "llama-3.1-8b-instant",
        "llama-3.1-8b-instant - fastest simple cleanup",
    ),
    (
        "meta-llama/llama-4-scout-17b-16e-instruct",
        "llama-4-scout-17b - preview, not preferred for Danish",
    ),
    (
        "groq/compound-mini",
        "groq/compound-mini - agentic, not cleanup default",
    ),
    (
        "groq/compound",
        "groq/compound - agentic, not cleanup default",
    ),
];
const OPENAI_POST_MODELS: &[&str] = &["gpt-4o-mini", "gpt-4o", "gpt-4.1-mini"];
const PARAKEET_MODELS: &[&str] = &[
    "",
    "nvidia/parakeet-tdt-0.6b-v3",
    "nvidia/parakeet-tdt-1.1b",
    "nvidia/parakeet-tdt-0.6b-v2",
];

pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 760.0])
            // Floor the window so the top status bar can't be squeezed until the
            // Start/Stop controls overlap the Status/Backend/Model cards. Below
            // this width there isn't room for the sidebar + all status cards +
            // the runtime controls on one row.
            .with_min_inner_size([1000.0, 640.0])
            .with_app_id("whisper-dictate")
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        &format!("whisper-dictate {}", runtime::version()),
        options,
        Box::new(|cc| {
            egui_material_icons::initialize(&cc.egui_ctx);
            Ok(Box::new(WhisperDictateApp::default()))
        }),
    )
    .map_err(|err| anyhow::anyhow!(err.to_string()))
}

struct WhisperDictateApp {
    app_version: String,
    selected_tab: Tab,
    runtime_state: RuntimeState,
    runtime_log: String,
    runtime_log_scroll_to_bottom: bool,
    runtime_log_view: LogViewMode,
    audio_capture_opening: bool,
    audio_capture_active: bool,
    audio_meter_level: f32,
    audio_meter_raw_dbfs: Option<f32>,
    audio_meter_peak: Option<f32>,
    active_audio_device: String,
    config_path: String,
    settings: AppSettings,
    saved_settings: AppSettings,
    settings_status: String,
    stt_api_key_input: String,
    saved_stt_api_key_input: String,
    stt_api_key_reveal_until: Option<Instant>,
    stt_api_key_status: String,
    post_api_key_input: String,
    saved_post_api_key_input: String,
    post_api_key_reveal_until: Option<Instant>,
    post_api_key_status: String,
    dictionary_preview: String,
    history_preview: String,
    metrics_preview: String,
    supervisor: RuntimeSupervisor,
    background_task: Option<Receiver<BackgroundTaskResult>>,
    background_task_label: Option<&'static str>,
    /// Best total VRAM (MB) of the detected NVIDIA GPU, or None on CPU /
    /// non-NVIDIA. Detected once at startup; gates the Whisper model picker.
    gpu_total_mb: Option<u32>,
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
        let (post_api_key_input, saved_post_api_key_input, post_api_key_status) =
            load_post_api_key_state(PostProvider::from_settings(&settings)).unwrap_or_else(|err| {
                (
                    String::new(),
                    String::new(),
                    format!("Could not load post-processing API key: {err}"),
                )
            });
        let config_path = config::config_path().display().to_string();
        let runtime_log = format!(
            "Rust UI ready. Start launches the Python dictation worker directly.\n[ui] config: {config_path}\n[ui] cloud API key load: {stt_api_key_status}\n[ui] post API key load: {post_api_key_status}"
        );
        Self {
            app_version: runtime::version(),
            selected_tab: Tab::Log,
            runtime_state: RuntimeState::Stopped,
            runtime_log,
            runtime_log_scroll_to_bottom: true,
            runtime_log_view: LogViewMode::from_raw(&settings.ui_log_view),
            audio_capture_opening: false,
            audio_capture_active: false,
            audio_meter_level: 0.0,
            audio_meter_raw_dbfs: None,
            audio_meter_peak: None,
            active_audio_device: String::new(),
            config_path,
            saved_settings: settings.clone(),
            settings,
            settings_status,
            saved_stt_api_key_input,
            stt_api_key_input,
            stt_api_key_reveal_until: None,
            stt_api_key_status,
            saved_post_api_key_input,
            post_api_key_input,
            post_api_key_reveal_until: None,
            post_api_key_status,
            dictionary_preview: String::new(),
            history_preview: String::new(),
            metrics_preview: String::new(),
            supervisor: RuntimeSupervisor::new(),
            background_task: None,
            background_task_label: None,
            gpu_total_mb: best_gpu_total_mb(),
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
pub(in crate::ui) enum Tab {
    Log,
    Speech,
    Quality,
    Dictionary,
    Output,
    Post,
    Profiles,
}

impl Tab {
    pub(in crate::ui) const ALL: [Tab; 7] = [
        Tab::Log,
        Tab::Speech,
        Tab::Quality,
        Tab::Dictionary,
        Tab::Output,
        Tab::Post,
        Tab::Profiles,
    ];

    pub(in crate::ui) fn label(self, raw_language: &str) -> &'static str {
        match self {
            Tab::Log => ui_text(raw_language, UiTextKey::Log),
            Tab::Speech => ui_text(raw_language, UiTextKey::Speech),
            Tab::Quality => ui_text(raw_language, UiTextKey::Quality),
            Tab::Dictionary => ui_text(raw_language, UiTextKey::Dictionary),
            Tab::Output => ui_text(raw_language, UiTextKey::Output),
            Tab::Post => ui_text(raw_language, UiTextKey::Post),
            Tab::Profiles => ui_text(raw_language, UiTextKey::Profiles),
        }
    }

    pub(in crate::ui) fn icon(self) -> &'static str {
        match self {
            Tab::Log => egui_material_icons::icons::ICON_ARTICLE,
            Tab::Speech => egui_material_icons::icons::ICON_MIC,
            Tab::Quality => egui_material_icons::icons::ICON_GRAPHIC_EQ,
            Tab::Dictionary => egui_material_icons::icons::ICON_BOOK,
            Tab::Output => egui_material_icons::icons::ICON_OUTPUT,
            Tab::Post => egui_material_icons::icons::ICON_AUTO_FIX_HIGH,
            Tab::Profiles => egui_material_icons::icons::ICON_GROUP,
        }
    }
}

#[cfg(test)]
mod api_key_env_tests;
#[cfg(test)]
mod api_key_store_tests;
#[cfg(test)]
mod backend_option_tests;
#[cfg(test)]
mod cloud_settings_tests;
#[cfg(test)]
mod keyboard_layout_tests;
#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod log_view_tests;
#[cfg(test)]
mod model_picker_tests;
#[cfg(test)]
mod settings_reset_tests;
#[cfg(test)]
mod tab_helpers_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod ui_language_tests;
