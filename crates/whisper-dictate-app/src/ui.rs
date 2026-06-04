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
    RuntimeState, RuntimeSupervisor, WorkerCommand,
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
            .with_app_id("whisper-dictate")
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
    runtime_log_scroll_to_bottom: bool,
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
            selected_tab: Tab::Runtime,
            runtime_state: RuntimeState::Stopped,
            runtime_log,
            runtime_log_scroll_to_bottom: true,
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
    fn start_runtime(&mut self) {
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            return;
        }
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
        self.ensure_stt_api_key_loaded_for_runtime();
        if self.cloud_stt_missing_api_key() {
            return;
        }
        let command = self.worker_command();
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

    fn ensure_stt_api_key_loaded_for_runtime(&mut self) {
        if self.settings.stt_backend != "openai" || !self.stt_api_key_input.trim().is_empty() {
            return;
        }
        self.reload_stt_api_key();
        if self.stt_api_key_input.trim().is_empty() {
            let provider = self.current_cloud_provider();
            let message = format!(
                "No {} API key loaded. Paste one in Core and click Save API key before starting cloud STT.",
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
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add(
                egui::TextEdit::singleline(value)
                    .password(!is_revealed)
                    .desired_width(332.0),
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

#[cfg(test)]
mod tests;
