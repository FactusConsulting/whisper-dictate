use anyhow::Result;
use eframe::egui;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::cloud_api::{check_cloud_api, check_post_api, CloudApiCheck, PostApiCheck};
use crate::config::{self, AppSettings};
use crate::dictionary;
use crate::runtime::{
    self, default_worker_command, doctor_command, install_command, run_capture, RuntimeEvent,
    RuntimeState, RuntimeSupervisor, WorkerCommand, WorkerEvent,
};
use crate::telemetry;

mod api_keys;
mod icon;
mod tabs;

use self::api_keys::*;
use self::icon::app_icon;

const XKB_LAYOUT_ENV: &str = "VOICEPI_XKB_LAYOUT";
const SUPPORTED_XKB_LAYOUTS: &[&str] = &[
    "dk", "no", "se", "de", "fi", "es", "pt", "br", "pl", "ua", "us",
];

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
const DEFAULT_UI_TEXT_SCALE: f32 = 1.15;
const UI_BG: egui::Color32 = egui::Color32::from_rgb(13, 18, 24);
const UI_PANEL_BG: egui::Color32 = egui::Color32::from_rgb(18, 25, 33);
const UI_HEADER_BG: egui::Color32 = egui::Color32::from_rgb(16, 24, 32);
const UI_SURFACE_BG: egui::Color32 = egui::Color32::from_rgb(24, 34, 45);
const UI_SURFACE_HOVER_BG: egui::Color32 = egui::Color32::from_rgb(31, 45, 59);
const UI_SURFACE_ACTIVE_BG: egui::Color32 = egui::Color32::from_rgb(35, 56, 72);
const UI_BORDER: egui::Color32 = egui::Color32::from_rgb(48, 64, 79);
const UI_BORDER_SOFT: egui::Color32 = egui::Color32::from_rgb(38, 51, 65);
const UI_TEXT: egui::Color32 = egui::Color32::from_rgb(226, 235, 243);
const UI_TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(143, 160, 176);
const UI_ACCENT_BLUE: egui::Color32 = egui::Color32::from_rgb(125, 211, 252);
const UI_ACCENT_DARK: egui::Color32 = egui::Color32::from_rgb(14, 84, 112);
const UI_SELECTION_BG: egui::Color32 = egui::Color32::from_rgb(12, 92, 123);
const UI_OK_TEXT: egui::Color32 = egui::Color32::from_rgb(110, 231, 183);
const UI_WARN_TEXT: egui::Color32 = egui::Color32::from_rgb(251, 191, 36);
const UI_ERROR_TEXT: egui::Color32 = egui::Color32::from_rgb(251, 113, 133);
const UI_LIGHT_BG: egui::Color32 = egui::Color32::from_rgb(238, 244, 250);
const UI_LIGHT_PANEL_BG: egui::Color32 = egui::Color32::from_rgb(248, 251, 254);
const UI_LIGHT_HEADER_BG: egui::Color32 = egui::Color32::from_rgb(226, 239, 249);
const UI_LIGHT_SURFACE_BG: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);
const UI_LIGHT_SURFACE_HOVER_BG: egui::Color32 = egui::Color32::from_rgb(230, 242, 252);
const UI_LIGHT_SURFACE_ACTIVE_BG: egui::Color32 = egui::Color32::from_rgb(204, 228, 246);
const UI_LIGHT_BORDER: egui::Color32 = egui::Color32::from_rgb(174, 194, 212);
const UI_LIGHT_BORDER_SOFT: egui::Color32 = egui::Color32::from_rgb(205, 219, 232);
const UI_LIGHT_TEXT: egui::Color32 = egui::Color32::from_rgb(28, 39, 52);
const UI_LIGHT_TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(86, 102, 119);
const UI_LIGHT_ACCENT_BLUE: egui::Color32 = egui::Color32::from_rgb(14, 116, 144);
const UI_LIGHT_ACCENT_DARK: egui::Color32 = egui::Color32::from_rgb(194, 225, 244);
const UI_LIGHT_SELECTION_BG: egui::Color32 = egui::Color32::from_rgb(191, 219, 254);
const UI_LIGHT_OK_TEXT: egui::Color32 = egui::Color32::from_rgb(21, 128, 61);
const UI_LIGHT_WARN_TEXT: egui::Color32 = egui::Color32::from_rgb(180, 83, 9);
const UI_LIGHT_ERROR_TEXT: egui::Color32 = egui::Color32::from_rgb(190, 18, 60);
const SIDEBAR_WIDTH: f32 = 164.0;
const TOP_STATUS_HEIGHT: f32 = 64.0;
const CONTROL_RADIUS: u8 = 8;
const PANEL_RADIUS: u8 = 12;
const PILL_RADIUS: u8 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiThemeMode {
    Dark,
    Light,
}

impl UiThemeMode {
    fn from_raw(raw: &str) -> Self {
        match raw {
            "light" => Self::Light,
            _ => Self::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct UiPalette {
    bg: egui::Color32,
    panel_bg: egui::Color32,
    header_bg: egui::Color32,
    surface_bg: egui::Color32,
    surface_hover_bg: egui::Color32,
    surface_active_bg: egui::Color32,
    border: egui::Color32,
    border_soft: egui::Color32,
    text: egui::Color32,
    text_muted: egui::Color32,
    accent_blue: egui::Color32,
    accent_dark: egui::Color32,
    selection_bg: egui::Color32,
    ok_text: egui::Color32,
    warn_text: egui::Color32,
    error_text: egui::Color32,
}

fn ui_palette(raw_theme: &str) -> UiPalette {
    match UiThemeMode::from_raw(raw_theme) {
        UiThemeMode::Dark => UiPalette {
            bg: UI_BG,
            panel_bg: UI_PANEL_BG,
            header_bg: UI_HEADER_BG,
            surface_bg: UI_SURFACE_BG,
            surface_hover_bg: UI_SURFACE_HOVER_BG,
            surface_active_bg: UI_SURFACE_ACTIVE_BG,
            border: UI_BORDER,
            border_soft: UI_BORDER_SOFT,
            text: UI_TEXT,
            text_muted: UI_TEXT_MUTED,
            accent_blue: UI_ACCENT_BLUE,
            accent_dark: UI_ACCENT_DARK,
            selection_bg: UI_SELECTION_BG,
            ok_text: UI_OK_TEXT,
            warn_text: UI_WARN_TEXT,
            error_text: UI_ERROR_TEXT,
        },
        UiThemeMode::Light => UiPalette {
            bg: UI_LIGHT_BG,
            panel_bg: UI_LIGHT_PANEL_BG,
            header_bg: UI_LIGHT_HEADER_BG,
            surface_bg: UI_LIGHT_SURFACE_BG,
            surface_hover_bg: UI_LIGHT_SURFACE_HOVER_BG,
            surface_active_bg: UI_LIGHT_SURFACE_ACTIVE_BG,
            border: UI_LIGHT_BORDER,
            border_soft: UI_LIGHT_BORDER_SOFT,
            text: UI_LIGHT_TEXT,
            text_muted: UI_LIGHT_TEXT_MUTED,
            accent_blue: UI_LIGHT_ACCENT_BLUE,
            accent_dark: UI_LIGHT_ACCENT_DARK,
            selection_bg: UI_LIGHT_SELECTION_BG,
            ok_text: UI_LIGHT_OK_TEXT,
            warn_text: UI_LIGHT_WARN_TEXT,
            error_text: UI_LIGHT_ERROR_TEXT,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SttBackendMode {
    Whisper,
    Parakeet,
    Cloud,
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 760.0])
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
            runtime_log_view: LogViewMode::Minimal,
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
    Log,
    Speech,
    Quality,
    Dictionary,
    Output,
    Post,
    Profiles,
}

impl Tab {
    const ALL: [Tab; 7] = [
        Tab::Log,
        Tab::Speech,
        Tab::Quality,
        Tab::Dictionary,
        Tab::Output,
        Tab::Post,
        Tab::Profiles,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Log => "Log",
            Tab::Speech => "Speech",
            Tab::Quality => "Quality",
            Tab::Dictionary => "Dictionary",
            Tab::Output => "Output",
            Tab::Post => "Post",
            Tab::Profiles => "Profiles",
        }
    }

    fn icon(self) -> &'static str {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogViewMode {
    Minimal,
    Diagnostic,
    Debug,
}

impl LogViewMode {
    const ALL: [LogViewMode; 3] = [
        LogViewMode::Minimal,
        LogViewMode::Diagnostic,
        LogViewMode::Debug,
    ];

    fn label(self) -> &'static str {
        match self {
            LogViewMode::Minimal => "Minimal",
            LogViewMode::Diagnostic => "Diagnostic",
            LogViewMode::Debug => "Debug",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeLogCardKind {
    FinalText,
    Status,
    Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeLogCard {
    kind: RuntimeLogCardKind,
    title: String,
    detail: String,
    badge: String,
}

impl eframe::App for WhisperDictateApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_runtime();
        self.poll_background_task();
        let palette = ui_palette(&self.settings.ui_theme);
        apply_ui_theme(ctx, &self.settings.ui_text_scale, &self.settings.ui_theme);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        paint_sidebar_bridge(ctx, palette, &self.settings.ui_text_scale);

        egui::SidePanel::left("primary_navigation")
            .resizable(false)
            .show_separator_line(false)
            .exact_width(sidebar_width(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.header_bg)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(14.0, 14.0)),
            )
            .show(ctx, |ui| self.sidebar(ui, palette));

        egui::TopBottomPanel::top("runtime_status")
            .resizable(false)
            .exact_height(top_status_bar_height(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.panel_bg)
                    .stroke(egui::Stroke::new(0.8, palette.border_soft))
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| self.top_status_bar(ui, palette));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(palette.panel_bg)
                    .inner_margin(egui::Margin::symmetric(12.0, 12.0)),
            )
            .show(ctx, |ui| match self.selected_tab {
                Tab::Log => self.runtime_tab(ui),
                Tab::Speech => self.settings_panel(ui, Self::core_tab),
                Tab::Quality => self.settings_panel(ui, Self::quality_tab),
                Tab::Dictionary => self.settings_panel(ui, Self::dictionary_tab),
                Tab::Output => self.settings_panel(ui, Self::output_tab),
                Tab::Post => self.settings_panel(ui, Self::post_processing_tab),
                Tab::Profiles => self.settings_panel(ui, Self::profiles_tab),
            });
    }
}

impl WhisperDictateApp {
    fn start_runtime(&mut self) {
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            return;
        }
        self.clear_audio_meter_and_device();
        let command = self.worker_command();
        self.append_runtime_log(format!("[ui] starting: {}", command.display()));
        if let Err(err) = self.supervisor.start(command) {
            self.append_runtime_log(format!("[ui] start failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    fn stop_runtime(&mut self) {
        self.clear_audio_meter();
        self.append_runtime_log("[ui] stopping runtime");
        if let Err(err) = self.supervisor.stop() {
            self.append_runtime_log(format!("[ui] stop failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    fn restart_runtime(&mut self) {
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            return;
        }
        let command = self.worker_command();
        self.clear_audio_meter_and_device();
        self.append_runtime_log(format!("[ui] restarting: {}", command.display()));
        if let Err(err) = self.supervisor.restart(command) {
            self.append_runtime_log(format!("[ui] restart failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
    }

    fn worker_command(&self) -> WorkerCommand {
        let mut command = default_worker_command();
        if let Some(xkb_layout) = effective_xkb_layout(&self.settings) {
            command.env.push((XKB_LAYOUT_ENV.to_owned(), xkb_layout));
        }
        if self.settings.stt_backend == "openai" {
            let key = self.stt_api_key_input.trim();
            if !key.is_empty() {
                command
                    .env
                    .push((STT_API_KEY_ENV.to_owned(), key.to_owned()));
            }
        }
        if matches!(self.settings.post_processor.as_str(), "openai" | "groq") {
            let post_key = self.post_api_key_input.trim();
            let key = if post_key.is_empty() {
                self.stt_api_key_input.trim()
            } else {
                post_key
            };
            if !key.is_empty() {
                command
                    .env
                    .push((POST_API_KEY_ENV.to_owned(), key.to_owned()));
            }
        }
        command
    }

    fn clear_audio_meter(&mut self) {
        self.audio_capture_opening = false;
        self.audio_capture_active = false;
        self.audio_meter_level = 0.0;
        self.audio_meter_raw_dbfs = None;
        self.audio_meter_peak = None;
    }

    fn clear_audio_meter_and_device(&mut self) {
        self.clear_audio_meter();
        self.active_audio_device.clear();
    }

    fn ensure_stt_api_key_loaded_for_runtime(&mut self) {
        if self.settings.stt_backend != "openai" || !self.stt_api_key_input.trim().is_empty() {
            return;
        }
        self.reload_stt_api_key();
        if self.stt_api_key_input.trim().is_empty() {
            let provider = self.current_cloud_provider();
            let message = format!(
                "No {} API key loaded. Paste one in Speech and click Save API key before starting cloud STT.",
                provider.label()
            );
            self.stt_api_key_status = message.clone();
            self.append_runtime_log(format!("[ui] {message}"));
        }
    }

    fn cloud_stt_missing_api_key(&self) -> bool {
        self.settings.stt_backend == "openai" && self.stt_api_key_input.trim().is_empty()
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
                self.stt_api_key_status = format!("[ERROR] Cloud API check failed: {err}");
                self.append_runtime_log(format!("[ERROR] cloud API check failed: {err}"));
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

    fn run_post_api_check(&mut self) {
        if self.background_task.is_some() {
            self.append_runtime_log("[ui] post API check skipped: another task is running");
            return;
        }

        let key = self.effective_post_api_key();
        let check = match PostApiCheck::from_settings(&self.settings, &key) {
            Ok(check) => check,
            Err(err) => {
                self.post_api_key_status = format!("[ERROR] Post API check failed: {err}");
                self.append_runtime_log(format!("[ERROR] post API check failed: {err}"));
                return;
            }
        };
        self.append_runtime_log(format!(
            "[ui] post API check: {} {}",
            check.provider, check.model
        ));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = match check_post_api(&check) {
                Ok(result) => BackgroundTaskResult {
                    label: "post API check",
                    command: format!("{} /chat/completions", check.provider),
                    stdout: result.summary(),
                    stderr: String::new(),
                    success: true,
                    code: None,
                    error: None,
                },
                Err(err) => BackgroundTaskResult {
                    label: "post API check",
                    command: format!("{} /chat/completions", check.provider),
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
        self.background_task_label = Some("post API check");
    }

    fn effective_post_api_key(&self) -> String {
        let post_key = self.post_api_key_input.trim();
        if !post_key.is_empty() {
            return post_key.to_owned();
        }
        self.stt_api_key_input.trim().to_owned()
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
                let message = format!("[ERROR] {} failed to run: {error}", result.label);
                self.set_api_check_status(result.label, &message);
                self.append_runtime_log(message);
            } else if result.success {
                let detail = result.stdout.trim();
                let message = if detail.is_empty() {
                    format!("[OK] {} passed", result.label)
                } else {
                    format!("[OK] {} passed: {detail}", result.label)
                };
                self.set_api_check_status(result.label, &message);
                self.append_runtime_log(message);
            } else {
                let detail = result.stdout.trim();
                let mut message = format!(
                    "[ERROR] {} failed with code {}",
                    result.label,
                    result
                        .code
                        .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
                );
                if !detail.is_empty() {
                    message.push_str(": ");
                    message.push_str(detail);
                }
                self.set_api_check_status(result.label, &message);
                self.append_runtime_log(message);
            }
        }
    }

    fn set_api_check_status(&mut self, label: &str, message: &str) {
        match label {
            "cloud API check" => self.stt_api_key_status = message.to_owned(),
            "post API check" => self.post_api_key_status = message.to_owned(),
            _ => {}
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
                        self.update_worker_status(&event);
                        if let Some(line) = worker_status_log_line(&event) {
                            self.append_runtime_log(line);
                        }
                    } else if event.event == "audio" {
                        self.update_worker_audio(&event);
                    }
                }
                RuntimeEvent::Stdout(line) | RuntimeEvent::Stderr(line) => {
                    self.append_runtime_log(line);
                }
                RuntimeEvent::Exited { code } => {
                    self.clear_audio_meter();
                    self.append_runtime_log(format!(
                        "[ui] runtime exited with code {}",
                        code.map_or_else(|| "unknown".to_owned(), |c| c.to_string())
                    ));
                }
                RuntimeEvent::Error(message) => {
                    self.clear_audio_meter();
                    self.append_runtime_log(format!("[ui] runtime error: {message}"));
                }
            }
        }
        self.runtime_state = self.supervisor.state();
    }

    fn update_worker_status(&mut self, event: &WorkerEvent) {
        if let Some(audio_device) = worker_event_string(&event.payload, "audio_device") {
            self.active_audio_device = audio_device;
        }
        if let Some(state) = event.state.as_deref() {
            self.audio_capture_opening = state == "opening";
            if let Some(active) = audio_capture_active_for_worker_state(state) {
                self.audio_capture_active = active;
                if !active {
                    self.audio_meter_level = 0.0;
                    self.audio_meter_raw_dbfs = None;
                    self.audio_meter_peak = None;
                }
            }
        }
    }

    fn update_worker_audio(&mut self, event: &WorkerEvent) {
        if let Some(audio_device) = worker_event_string(&event.payload, "audio_device") {
            self.active_audio_device = audio_device;
        }
        if let Some(level) = worker_event_f32(&event.payload, "level") {
            self.audio_meter_level = level.clamp(0.0, 1.0);
        }
        if let Some(raw_dbfs) = worker_event_f32(&event.payload, "raw_dbfs") {
            self.audio_meter_raw_dbfs = Some(raw_dbfs);
        }
        if let Some(peak) = worker_event_f32(&event.payload, "peak") {
            self.audio_meter_peak = Some(peak);
        }
        if let Some(state) = event.state.as_deref() {
            self.audio_capture_opening = state == "opening";
            if let Some(active) = audio_capture_active_for_worker_state(state) {
                self.audio_capture_active = active;
                if !active {
                    self.audio_meter_level = 0.0;
                }
            }
        } else {
            self.audio_capture_active = true;
        }
    }

    fn append_runtime_log(&mut self, line: impl AsRef<str>) {
        if !self.runtime_log.is_empty() {
            self.runtime_log.push('\n');
        }
        self.runtime_log.push_str(line.as_ref());
        self.runtime_log_scroll_to_bottom = true;
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
                let post_key_message = self.save_post_api_key_if_changed();
                self.saved_settings = self.settings.clone();
                self.settings_status = format!("Saved settings: {}", path.display());
                self.append_runtime_log(format!("[ui] settings saved: {}", path.display()));
                if let Some(message) = key_message {
                    self.settings_status.push_str(" | ");
                    self.settings_status.push_str(&message);
                    self.append_runtime_log(format!("[ui] cloud API key save: {message}"));
                }
                if let Some(message) = post_key_message {
                    self.settings_status.push_str(" | ");
                    self.settings_status.push_str(&message);
                    self.append_runtime_log(format!("[ui] post API key save: {message}"));
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
            || self.post_api_key_input != self.saved_post_api_key_input
    }

    fn reload_settings(&mut self) {
        match config::load_settings() {
            Ok(settings) => {
                self.saved_settings = settings.clone();
                self.settings = settings;
                self.reload_stt_api_key();
                self.reload_post_api_key();
                self.settings_status = "Reloaded config".to_owned();
                self.append_runtime_log(format!("[ui] settings loaded: {}", self.config_path));
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
                if !labeled_options_contain(GROQ_POST_MODELS, &self.settings.post_model) {
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

    fn reload_post_api_key(&mut self) {
        match load_post_api_key_state(PostProvider::from_settings(&self.settings)) {
            Ok((key, saved_key, status)) => {
                self.post_api_key_input = key;
                self.saved_post_api_key_input = saved_key;
                self.post_api_key_status = status;
            }
            Err(err) => {
                self.post_api_key_input.clear();
                self.saved_post_api_key_input.clear();
                self.post_api_key_status = format!("Could not load post-processing API key: {err}");
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
            Ok(report) => {
                self.saved_stt_api_key_input = self.stt_api_key_input.clone();
                if self.stt_api_key_input.trim().is_empty() {
                    format!("Cleared saved {} API key.", provider.label())
                } else {
                    format!(
                        "Saved {} API key in {}.",
                        provider.label(),
                        report.status_label()
                    )
                }
            }
            Err(err) => {
                format!("Could not save {} API key: {err}", provider.label())
            }
        };
        self.stt_api_key_status = message.clone();
        Some(message)
    }

    fn save_stt_api_key_now(&mut self) {
        if self.settings.stt_backend != "openai" {
            self.stt_api_key_status =
                "API keys are only used when STT backend is Cloud STT.".to_owned();
            return;
        }
        let provider = self.current_cloud_provider();
        self.apply_cloud_provider_defaults(provider);
        let mut key_log_details = None;
        let key_message = match save_stt_api_key(provider, self.stt_api_key_input.trim()) {
            Ok(report) => {
                key_log_details = Some(report.log_details());
                self.saved_stt_api_key_input = self.stt_api_key_input.clone();
                if self.stt_api_key_input.trim().is_empty() {
                    format!(
                        "Cleared saved {} API key. {}",
                        provider.label(),
                        report.status_label()
                    )
                } else {
                    format!(
                        "Saved {} API key in {}.",
                        provider.label(),
                        report.status_label()
                    )
                }
            }
            Err(err) => {
                format!("Could not save {} API key: {err}", provider.label())
            }
        };
        match self.persist_cloud_provider_selection() {
            Ok(Some(path)) => {
                self.stt_api_key_status =
                    format!("{key_message} Saved provider settings: {}", path.display());
                self.append_runtime_log(format!(
                    "[ui] cloud API key save: {key_message}; {}; provider_settings={}",
                    key_log_details
                        .as_deref()
                        .unwrap_or("no secret save details"),
                    path.display()
                ));
            }
            Ok(None) => {
                self.stt_api_key_status = key_message;
                self.append_runtime_log(format!(
                    "[ui] cloud API key save: {}; {}",
                    self.stt_api_key_status,
                    key_log_details
                        .as_deref()
                        .unwrap_or("no secret save details")
                ));
            }
            Err(err) => {
                self.stt_api_key_status =
                    format!("{key_message} Provider settings save failed: {err}");
                self.append_runtime_log(format!(
                    "[ERROR] cloud API key save: {}; provider settings save failed: {err}",
                    key_message
                ));
            }
        }
    }

    fn persist_cloud_provider_selection(&mut self) -> Result<Option<std::path::PathBuf>> {
        let provider = self.current_cloud_provider();
        let mut saved = self.saved_settings.clone();
        saved.stt_backend = "openai".to_owned();
        saved.stt_provider = provider.id().to_owned();
        saved.stt_base_url = provider.base_url().to_owned();
        saved.stt_model = self.settings.stt_model.clone();

        if saved == self.saved_settings {
            return Ok(None);
        }

        let path = config::save_settings(&saved)?;
        self.saved_settings.stt_backend = saved.stt_backend;
        self.saved_settings.stt_provider = saved.stt_provider;
        self.saved_settings.stt_base_url = saved.stt_base_url;
        self.saved_settings.stt_model = saved.stt_model;
        Ok(Some(path))
    }

    fn save_post_api_key_if_changed(&mut self) -> Option<String> {
        if self.post_api_key_input == self.saved_post_api_key_input {
            return None;
        }
        if PostProvider::from_settings(&self.settings).is_none()
            && self.post_api_key_input.is_empty()
        {
            return None;
        }
        let message = self.save_post_api_key_message();
        self.post_api_key_status = message.clone();
        Some(message)
    }

    fn save_post_api_key_now(&mut self) {
        self.post_api_key_status = self.save_post_api_key_message();
    }

    fn save_post_api_key_message(&mut self) -> String {
        let Some(provider) = PostProvider::from_settings(&self.settings) else {
            return "Post API keys are only used when Post processor is Groq or OpenAI.".to_owned();
        };
        match save_post_api_key(provider, self.post_api_key_input.trim()) {
            Ok(report) => {
                let log_details = report.log_details();
                self.saved_post_api_key_input = self.post_api_key_input.clone();
                let message = if self.post_api_key_input.trim().is_empty() {
                    format!("Cleared saved {} API key.", provider.label())
                } else {
                    format!(
                        "Saved {} API key in {}.",
                        provider.label(),
                        report.status_label()
                    )
                };
                self.append_runtime_log(format!(
                    "[ui] post API key save: {}; {}",
                    message, log_details
                ));
                message
            }
            Err(err) => format!("Could not save {} API key: {err}", provider.label()),
        }
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

fn password_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    reveal_until: &mut Option<Instant>,
    help: &str,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    let now = Instant::now();
    if reveal_until.is_some_and(|until| until <= now) {
        *reveal_until = None;
    }
    let is_revealed = reveal_until.is_some_and(|until| until > now);
    if let Some(until) = *reveal_until {
        ui.ctx()
            .request_repaint_after(until.saturating_duration_since(now));
    }
    ui.add_enabled_ui(enabled, |ui| {
        const PASSWORD_CONTROL_WIDTH: f32 = 360.0;
        const EYE_BUTTON_WIDTH: f32 = 26.0;
        const EYE_BUTTON_GAP: f32 = 4.0;
        let input_width = PASSWORD_CONTROL_WIDTH - EYE_BUTTON_WIDTH - EYE_BUTTON_GAP;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = EYE_BUTTON_GAP;
            ui.set_width(PASSWORD_CONTROL_WIDTH);
            ui.add_sized(
                egui::vec2(input_width, 22.0),
                egui::TextEdit::singleline(value)
                    .password(!is_revealed)
                    .desired_width(input_width),
            );
            let response = eye_icon_button(ui, is_revealed).on_hover_text(if is_revealed {
                "Hide API key."
            } else {
                "Show API key for 3 seconds."
            });
            if response.clicked() {
                *reveal_until = if is_revealed {
                    None
                } else {
                    Some(Instant::now() + Duration::from_secs(3))
                };
            }
        });
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn eye_icon_button(ui: &mut egui::Ui, active: bool) -> egui::Response {
    let size = egui::vec2(26.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        ui.painter()
            .rect(rect, 2.0, visuals.bg_fill, visuals.bg_stroke);

        let stroke = egui::Stroke::new(
            1.3,
            if active {
                ui.visuals().selection.stroke.color
            } else {
                visuals.fg_stroke.color
            },
        );
        let center = rect.center();
        let left = egui::pos2(rect.left() + 5.0, center.y);
        let right = egui::pos2(rect.right() - 5.0, center.y);
        let top = egui::pos2(center.x, rect.top() + 6.0);
        let bottom = egui::pos2(center.x, rect.bottom() - 6.0);
        ui.painter().line_segment([left, top], stroke);
        ui.painter().line_segment([top, right], stroke);
        ui.painter().line_segment([right, bottom], stroke);
        ui.painter().line_segment([bottom, left], stroke);
        ui.painter().circle_stroke(center, 2.4, stroke);
        if active {
            ui.painter()
                .circle_filled(center, 1.4, ui.visuals().selection.stroke.color);
        }
    }
    response
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

fn combo_help_labeled(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    options: &[(&str, &str)],
    help: &str,
) {
    let show_help = label_with_help(ui, label, help);
    egui::ComboBox::from_id_salt(label)
        .selected_text(selected_option_label(value, options))
        .show_ui(ui, |ui| {
            for (option, display) in options {
                ui.selectable_value(value, (*option).to_owned(), *display);
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

fn combo_enabled_labeled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[(&str, &str)],
    help: &str,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(label)
            .selected_text(selected_option_label(value, options))
            .show_ui(ui, |ui| {
                for (option, display) in options {
                    ui.selectable_value(value, (*option).to_owned(), *display);
                }
            });
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn selected_option_label(value: &str, options: &[(&str, &str)]) -> String {
    options
        .iter()
        .find(|(option, _)| *option == value)
        .map(|(_, display)| (*display).to_owned())
        .unwrap_or_else(|| {
            if value.is_empty() {
                "(empty)".to_owned()
            } else {
                value.to_owned()
            }
        })
}

fn labeled_options_contain(options: &[(&str, &str)], value: &str) -> bool {
    options.iter().any(|(option, _)| *option == value)
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
        ui.add(
            egui::Label::new(egui::RichText::new(help).color(ui.visuals().weak_text_color()))
                .wrap(),
        );
    }
}

fn apply_ui_theme(ctx: &egui::Context, raw_scale: &str, raw_theme: &str) {
    let theme = UiThemeMode::from_raw(raw_theme);
    let palette = ui_palette(raw_theme);
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
    let button_padding = egui::vec2(10.0 * scale, 5.0 * scale);
    let item_spacing = egui::vec2(9.0 * scale, 7.0 * scale);
    let mut style = (*ctx.style()).clone();
    style.text_styles = text_styles;
    style.spacing.button_padding = button_padding;
    style.spacing.item_spacing = item_spacing;
    style.spacing.interact_size = egui::vec2(42.0 * scale, 28.0 * scale);
    style.visuals = themed_visuals(theme, palette);
    ctx.set_style(style);
}

fn themed_visuals(theme: UiThemeMode, palette: UiPalette) -> egui::Visuals {
    let mut visuals = match theme {
        UiThemeMode::Dark => egui::Visuals::dark(),
        UiThemeMode::Light => egui::Visuals::light(),
    };
    visuals.override_text_color = Some(palette.text);
    visuals.panel_fill = palette.panel_bg;
    visuals.window_fill = palette.panel_bg;
    visuals.faint_bg_color = palette.surface_bg;
    visuals.extreme_bg_color = palette.bg;
    visuals.code_bg_color = palette.bg;
    visuals.hyperlink_color = palette.accent_blue;
    visuals.warn_fg_color = palette.warn_text;
    visuals.error_fg_color = palette.error_text;
    visuals.selection.bg_fill = palette.selection_bg;
    visuals.selection.stroke = egui::Stroke::new(1.0, palette.accent_blue);
    visuals.widgets.noninteractive.bg_fill = palette.panel_bg;
    visuals.widgets.noninteractive.weak_bg_fill = palette.panel_bg;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(0.8, palette.border_soft);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, palette.text_muted);
    visuals.widgets.inactive.bg_fill = palette.surface_bg;
    visuals.widgets.inactive.weak_bg_fill = palette.surface_bg;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(0.8, palette.border_soft);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, palette.text);
    visuals.widgets.hovered.bg_fill = palette.surface_hover_bg;
    visuals.widgets.hovered.weak_bg_fill = palette.surface_hover_bg;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette.accent_blue);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, palette.text);
    visuals.widgets.active.bg_fill = palette.surface_active_bg;
    visuals.widgets.active.weak_bg_fill = palette.surface_active_bg;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, palette.accent_blue);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, palette.text);
    visuals.widgets.open.bg_fill = palette.accent_dark;
    visuals.widgets.open.weak_bg_fill = palette.accent_dark;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, palette.accent_blue);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, palette.text);
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.widgets.inactive.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.widgets.hovered.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.widgets.active.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.widgets.open.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.window_rounding = egui::Rounding::same(PANEL_RADIUS as f32);
    visuals
}

fn nav_button(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    label: &str,
    palette: UiPalette,
) -> egui::Response {
    let fill = if selected {
        palette.accent_dark
    } else {
        egui::Color32::TRANSPARENT
    };
    let stroke = if selected {
        egui::Stroke::new(1.0, palette.accent_blue)
    } else {
        egui::Stroke::NONE
    };
    let text = if selected {
        icon_text(icon, label)
            .size(15.0)
            .strong()
            .color(palette.text)
    } else {
        icon_text(icon, label).size(15.0).color(palette.text_muted)
    };
    ui.add_sized(
        egui::vec2(ui.available_width(), 38.0),
        egui::Button::new(text).fill(fill).stroke(stroke),
    )
}

fn icon_text(icon: &str, label: impl AsRef<str>) -> egui::RichText {
    egui::RichText::new(format!("{icon}  {}", label.as_ref()))
}

fn sidebar_width(raw_scale: &str) -> f32 {
    let scale = raw_scale.parse::<f32>().unwrap_or(1.0).clamp(0.85, 1.6);
    SIDEBAR_WIDTH * scale
}

fn paint_sidebar_bridge(ctx: &egui::Context, palette: UiPalette, raw_scale: &str) {
    let screen = ctx.screen_rect();
    let left = screen.left() + sidebar_width(raw_scale) - 1.0;
    let bridge = egui::Rect::from_min_max(
        egui::pos2(left, screen.top()),
        egui::pos2((left + 16.0).min(screen.right()), screen.bottom()),
    );
    ctx.layer_painter(egui::LayerId::background())
        .rect_filled(bridge, 0.0, palette.panel_bg);
}

fn top_status_bar_height(raw_scale: &str) -> f32 {
    let scale = raw_scale.parse::<f32>().unwrap_or(1.0).clamp(0.85, 1.6);
    TOP_STATUS_HEIGHT * scale
}

fn panel_frame(palette: UiPalette) -> egui::Frame {
    egui::Frame::default()
        .fill(palette.surface_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
}

fn inset_panel_frame(palette: UiPalette) -> egui::Frame {
    egui::Frame::default()
        .fill(palette.bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
}

fn worker_status_log_line(event: &WorkerEvent) -> Option<String> {
    if event.event != "status" {
        return None;
    }
    let state = event.state.as_deref().unwrap_or("unknown");
    let mut line = format!("[worker] status={state}");
    for key in [
        "backend",
        "model",
        "device",
        "compute_type",
        "capture_backend",
        "capture_channels",
        "audio_device",
        "startup_ms",
        "first_audio",
        "recording_s",
    ] {
        if let Some(value) = worker_event_string(&event.payload, key) {
            line.push(' ');
            line.push_str(key);
            line.push('=');
            line.push_str(&value);
        }
    }
    Some(line)
}

fn worker_event_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    let value = payload.get(key)?;
    if let Some(raw) = value.as_str() {
        let raw = raw.trim();
        return (!raw.is_empty()).then(|| raw.to_owned());
    }
    if value.is_number() || value.is_boolean() {
        return Some(value.to_string());
    }
    None
}

fn worker_event_f32(payload: &serde_json::Value, key: &str) -> Option<f32> {
    let value = payload.get(key)?;
    if let Some(raw) = value.as_f64() {
        return Some(raw as f32);
    }
    value.as_str()?.trim().parse::<f32>().ok()
}

fn audio_capture_active_for_worker_state(state: &str) -> Option<bool> {
    match state {
        "recording" | "listening" => Some(true),
        "opening" | "ready" | "transcribing" | "loading_model" | "failed" => Some(false),
        _ => None,
    }
}

fn log_view_text(log: &str, mode: LogViewMode) -> String {
    match mode {
        LogViewMode::Minimal => final_output_text(log),
        LogViewMode::Debug => log.to_owned(),
        LogViewMode::Diagnostic => log
            .lines()
            .filter(|line| is_diagnostic_log_line(line))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn runtime_log_cards(log: &str, mode: LogViewMode) -> Vec<RuntimeLogCard> {
    if matches!(mode, LogViewMode::Debug) {
        return Vec::new();
    }

    let mut cards = Vec::new();
    for line in log.lines() {
        if let Some(text) = extract_inject_preview(line) {
            cards.push(RuntimeLogCard {
                kind: RuntimeLogCardKind::FinalText,
                title: text,
                detail: if matches!(mode, LogViewMode::Diagnostic) {
                    latest_previous_post_detail(log, line)
                        .unwrap_or_else(|| "Final output".to_owned())
                } else {
                    String::new()
                },
                badge: "Final".to_owned(),
            });
            continue;
        }

        if matches!(mode, LogViewMode::Minimal) {
            continue;
        }

        if line.starts_with("[post]") {
            cards.push(RuntimeLogCard {
                kind: RuntimeLogCardKind::Status,
                title: strip_log_prefix(line).to_owned(),
                detail: "Post-processing".to_owned(),
                badge: "Post".to_owned(),
            });
            continue;
        }

        if line.starts_with("[worker] status=") {
            cards.push(RuntimeLogCard {
                kind: RuntimeLogCardKind::Status,
                title: line.trim_start_matches("[worker] status=").to_owned(),
                detail: "Worker state".to_owned(),
                badge: "Worker".to_owned(),
            });
            continue;
        }

        if line.starts_with("[OK]") || line.starts_with("[ERROR]") {
            cards.push(RuntimeLogCard {
                kind: RuntimeLogCardKind::Status,
                title: line.to_owned(),
                detail: "Runtime message".to_owned(),
                badge: "Status".to_owned(),
            });
            continue;
        }

        if matches!(mode, LogViewMode::Diagnostic) && is_diagnostic_detail_line(line) {
            cards.push(RuntimeLogCard {
                kind: RuntimeLogCardKind::Diagnostic,
                title: compact_diagnostic_title(line),
                detail: diagnostic_detail_label(line).to_owned(),
                badge: diagnostic_badge(line).to_owned(),
            });
        }
    }
    cards
}

fn final_output_text(log: &str) -> String {
    log.lines()
        .filter_map(extract_inject_preview)
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_diagnostic_status_line(line: &str) -> bool {
    line.starts_with("[worker] status=")
        || line.starts_with("[post]")
        || line.starts_with("[inject]")
        || line.starts_with("[OK]")
        || line.starts_with("[ERROR]")
}

fn is_diagnostic_log_line(line: &str) -> bool {
    is_diagnostic_status_line(line) || is_diagnostic_detail_line(line)
}

fn is_diagnostic_detail_line(line: &str) -> bool {
    line.starts_with("[gate]")
        || line.starts_with("[cap]")
        || line.starts_with("[stt]")
        || line.starts_with("[stt-debug]")
}

fn extract_inject_preview(line: &str) -> Option<String> {
    if !line.starts_with("[inject]") {
        return None;
    }
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    let text = rest[..end].trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn latest_previous_post_detail(log: &str, current_line: &str) -> Option<String> {
    let mut previous = None;
    for line in log.lines() {
        if line == current_line {
            break;
        }
        if line.starts_with("[post]") {
            previous = Some(strip_log_prefix(line).to_owned());
        }
    }
    previous
}

fn strip_log_prefix(line: &str) -> &str {
    line.split_once(']').map_or(line, |(_, rest)| rest.trim())
}

fn diagnostic_badge(line: &str) -> &str {
    if line.starts_with("[gate]") {
        "Gate"
    } else if line.starts_with("[cap]") {
        "Capture"
    } else if line.starts_with("[stt-debug]") {
        "STT debug"
    } else if line.starts_with("[stt]") {
        "STT"
    } else {
        "Diag"
    }
}

fn diagnostic_detail_label(line: &str) -> &str {
    if line.starts_with("[gate]") {
        "Voice gate"
    } else if line.starts_with("[cap]") {
        "Audio input"
    } else if line.starts_with("[stt-debug]") {
        "Backend detail"
    } else if line.starts_with("[stt]") {
        "Transcription"
    } else {
        "Diagnostic"
    }
}

fn compact_diagnostic_title(line: &str) -> String {
    if line.starts_with("[stt]") {
        let dur = extract_metric_token(line, "dur=").unwrap_or("duration=?");
        let compute = extract_metric_token(line, "compute=").unwrap_or("compute=?");
        let rtf = extract_metric_token(line, "rtf=").unwrap_or("rtf=?");
        return format!("{dur}  {compute}  {rtf}");
    }
    if line.starts_with("[cap]") || line.starts_with("[gate]") {
        let raw = extract_metric_token(line, "raw=").unwrap_or("raw=?");
        let snr = extract_metric_token(line, "snr=").unwrap_or("snr=?");
        let peak = extract_metric_token(line, "peak=");
        let input = extract_metric_token(line, "input=");
        return match (peak, input) {
            (Some(peak), Some(input)) => format!("{raw}  {peak}  {input}  {snr}"),
            (Some(peak), None) => format!("{raw}  {peak}  {snr}"),
            (None, Some(input)) => format!("{raw}  {input}  {snr}"),
            (None, None) => format!("{raw}  {snr}"),
        };
    }
    strip_log_prefix(line).to_owned()
}

fn extract_metric_token<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)?;
    let token = line[start..]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(',');
    (!token.is_empty()).then_some(token)
}

fn latest_prefixed_line<'a>(log: &'a str, prefix: &str) -> Option<&'a str> {
    log.lines().rev().find(|line| line.starts_with(prefix))
}

fn audio_meter_level(live_level: f32, state: RuntimeState, capture_active: bool) -> f32 {
    if state == RuntimeState::Stopped || !capture_active {
        return 0.0;
    }
    live_level.clamp(0.0, 1.0)
}

fn effective_xkb_layout(settings: &AppSettings) -> Option<String> {
    if let Some(configured) = normalize_xkb_layout(&settings.xkb_layout) {
        return Some(configured);
    }
    detect_gnome_xkb_layout()
}

fn normalize_xkb_layout(raw: &str) -> Option<String> {
    let layout = match raw.trim() {
        "da" => "dk",
        "sv" => "se",
        "nb" | "nn" => "no",
        "uk" => "ua",
        value => value,
    };
    if SUPPORTED_XKB_LAYOUTS.contains(&layout) {
        Some(layout.to_owned())
    } else {
        None
    }
}

fn detect_gnome_xkb_layout() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.input-sources", "sources"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    parse_gnome_xkb_sources(&raw)
}

fn parse_gnome_xkb_sources(raw: &str) -> Option<String> {
    for entry in raw.split('(').skip(1) {
        let Some(entry) = entry.split(')').next() else {
            continue;
        };
        let mut values = entry
            .split(',')
            .map(|part| part.trim().trim_matches('\'').trim_matches('"'));
        let kind = values.next().unwrap_or_default();
        let layout = values.next().unwrap_or_default();
        let layout = normalize_xkb_layout(layout);
        if kind == "xkb" && layout.as_deref().is_some_and(|value| value != "us") {
            return layout;
        }
    }
    None
}

fn open_url(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        command
            .args(["/C", "start", "", url])
            .creation_flags(0x08000000);
        command.spawn()?;
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
mod settings_reset_tests;
#[cfg(test)]
mod test_support;
