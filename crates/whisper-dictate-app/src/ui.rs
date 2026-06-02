use anyhow::Result;
use eframe::egui;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::config::{self, AppSettings};
use crate::dictionary;
use crate::runtime::{
    self, default_worker_command, doctor_command, install_command, run_capture, RuntimeEvent,
    RuntimeState, RuntimeSupervisor, WorkerCommand,
};

const GROQ_STT_BASE_URL: &str = "https://api.groq.com/openai/v1";
const GROQ_STT_MODEL: &str = "whisper-large-v3-turbo";
const GROQ_KEYS_URL: &str = "https://console.groq.com/keys";
const WHISPER_MODELS: &[&str] = &[
    "large-v3-turbo",
    "large-v3",
    "medium",
    "small",
    "base",
    "tiny",
];
const EXTERNAL_STT_MODELS: &[&str] = &[
    "",
    "whisper-large-v3-turbo",
    "whisper-large-v3",
    "distil-whisper-large-v3-en",
    "gpt-4o-mini-transcribe",
    "gpt-4o-transcribe",
    "whisper-1",
];
const PARAKEET_MODELS: &[&str] = &[
    "",
    "nvidia/parakeet-tdt-0.6b-v3",
    "nvidia/parakeet-tdt-1.1b",
    "nvidia/parakeet-tdt-0.6b-v2",
];

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
            .with_inner_size([920.0, 680.0])
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
    dictionary_preview: String,
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
        Self {
            selected_tab: Tab::Runtime,
            runtime_state: RuntimeState::Stopped,
            runtime_log: "Rust UI ready. Start launches the Python dictation worker directly."
                .to_owned(),
            config_path: config::config_path().display().to_string(),
            saved_settings: settings.clone(),
            settings,
            settings_status,
            dictionary_preview: String::new(),
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
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let height = (ui.available_height() - 8.0).max(240.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.runtime_log)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(ui.available_width())
                        .desired_rows(28)
                        .min_size(egui::vec2(ui.available_width(), height))
                        .interactive(false),
                );
            });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui, body: fn(&mut Self, &mut egui::Ui)) {
        body(self, ui);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                self.save_settings();
            }
            if ui.button("Reload").clicked() {
                self.reload_settings();
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
                    "Cloud STT model",
                    &mut self.settings.stt_model,
                    EXTERNAL_STT_MODELS,
                    "Remote model name sent to the configured OpenAI-compatible STT API.",
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
                ui.end_row();
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
        ui.horizontal(|ui| {
            if ui.button("Use Groq cloud STT").clicked() {
                self.settings.stt_backend = "openai".to_owned();
                self.settings.stt_model = GROQ_STT_MODEL.to_owned();
                self.settings.stt_base_url = GROQ_STT_BASE_URL.to_owned();
                self.settings_status =
                    "Groq preset applied. Set GROQ_API_KEY, VOICEPI_STT_API_KEY or OPENAI_API_KEY before starting."
                        .to_owned();
            }
            if ui.button("Groq API keys").clicked() {
                match open_url(GROQ_KEYS_URL) {
                    Ok(()) => {
                        self.settings_status =
                            "Opened Groq API keys. Store the key in GROQ_API_KEY, VOICEPI_STT_API_KEY or OPENAI_API_KEY."
                                .to_owned();
                    }
                    Err(err) => {
                        self.settings_status =
                            format!("Could not open Groq API keys page: {err}");
                    }
                }
            }
        });
        if self.settings.stt_backend == "openai" {
            ui.label(
                "Cloud STT sends recorded audio to the configured provider. API keys are read from environment variables only.",
            );
        }
    }

    fn quality_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Quality");
        egui::Grid::new("quality_settings")
            .num_columns(2)
            .show(ui, |ui| {
                text(ui, "Beam size", &mut self.settings.beam_size);
                text(ui, "Temperature ladder", &mut self.settings.temperature);
                text(
                    ui,
                    "Context min seconds",
                    &mut self.settings.context_min_seconds,
                );
                text(
                    ui,
                    "Parakeet min seconds",
                    &mut self.settings.parakeet_min_seconds,
                );
                text(ui, "Release tail ms", &mut self.settings.release_tail_ms);
                text(ui, "VAD threshold", &mut self.settings.vad_threshold);
                text(
                    ui,
                    "VAD min silence ms",
                    &mut self.settings.vad_min_silence_ms,
                );
                text(ui, "Target dBFS", &mut self.settings.target_dbfs);
                text(ui, "Min input dBFS", &mut self.settings.min_input_dbfs);
                text(ui, "Min SNR dB", &mut self.settings.min_snr_db);
            });
        ui.label("Initial prompt");
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
                text(ui, "Dictionary path", &mut self.settings.dictionary);
                checkbox(
                    ui,
                    "Dictionary enabled",
                    &mut self.settings.dictionary_enabled,
                );
                text(
                    ui,
                    "Max prompt terms",
                    &mut self.settings.dictionary_max_terms,
                );
                text(
                    ui,
                    "Prompt char cap",
                    &mut self.settings.dictionary_prompt_chars,
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
                combo(
                    ui,
                    "Inject mode",
                    &mut self.settings.inject_mode,
                    &["auto", "type", "paste", "print"],
                );
                combo(
                    ui,
                    "Format commands",
                    &mut self.settings.format_commands,
                    &["off", "en", "da", "both"],
                );
                checkbox(ui, "JSON stdout", &mut self.settings.inject_json);
                text(ui, "Metrics JSONL", &mut self.settings.metrics_jsonl);
                text(ui, "Command hook", &mut self.settings.command_hook);
                text(
                    ui,
                    "Command hook timeout ms",
                    &mut self.settings.command_hook_timeout_ms,
                );
                combo(
                    ui,
                    "Post processor",
                    &mut self.settings.post_processor,
                    &["none", "ollama", "openai"],
                );
                combo(
                    ui,
                    "Post mode",
                    &mut self.settings.post_mode,
                    &[
                        "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
                    ],
                );
                text(ui, "Post model", &mut self.settings.post_model);
                text(ui, "Post base URL", &mut self.settings.post_base_url);
                text(ui, "Post timeout ms", &mut self.settings.post_timeout_ms);
                text(
                    ui,
                    "Post max input chars",
                    &mut self.settings.post_max_input_chars,
                );
                text(
                    ui,
                    "Post max output chars",
                    &mut self.settings.post_max_output_chars,
                );
                checkbox(ui, "History enabled", &mut self.settings.history_enabled);
                text(ui, "History JSONL", &mut self.settings.history_jsonl);
                checkbox(ui, "Local only", &mut self.settings.local_only);
                checkbox(ui, "VOICEPI_DEBUG", &mut self.settings.debug);
                checkbox(ui, "VOICEPI_STT_DEBUG", &mut self.settings.stt_debug);
            });
    }

    fn profiles_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        ui.label("Profiles JSON");
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.profiles_json)
                .font(egui::TextStyle::Monospace)
                .desired_rows(22)
                .desired_width(f32::INFINITY),
        );
    }

    fn start_runtime(&mut self) {
        let command = default_worker_command();
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
        let command = default_worker_command();
        self.append_runtime_log(format!("[ui] restarting: {}", command.display()));
        if let Err(err) = self.supervisor.restart(command) {
            self.append_runtime_log(format!("[ui] restart failed: {err}"));
        }
        self.runtime_state = self.supervisor.state();
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
        if let Err(err) = serde_json::from_str::<serde_json::Value>(&self.settings.profiles_json) {
            self.settings_status = format!("Profiles JSON is invalid: {err}");
            return;
        }
        match config::save_settings(&self.settings) {
            Ok(path) => {
                let restart_keys =
                    config::restart_required_keys(&self.saved_settings, &self.settings);
                self.saved_settings = self.settings.clone();
                self.settings_status = format!("Saved: {}", path.display());
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

    fn reload_settings(&mut self) {
        match config::load_settings() {
            Ok(settings) => {
                self.saved_settings = settings.clone();
                self.settings = settings;
                self.settings_status = "Reloaded config".to_owned();
            }
            Err(err) => {
                self.settings_status = format!("Reload failed: {err}");
            }
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

fn text(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).desired_width(360.0));
    ui.end_row();
}

fn text_help(ui: &mut egui::Ui, label: &str, value: &mut String, help: &str) {
    ui.label(label).on_hover_text(help);
    ui.add(egui::TextEdit::singleline(value).desired_width(360.0));
    ui.end_row();
}

fn text_enabled(ui: &mut egui::Ui, enabled: bool, label: &str, value: &mut String, help: &str) {
    ui.add_enabled(enabled, egui::Label::new(label))
        .on_hover_text(help);
    ui.add_enabled_ui(enabled, |ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(360.0));
    });
    ui.end_row();
}

fn checkbox(ui: &mut egui::Ui, label: &str, value: &mut bool) {
    ui.label(label);
    ui.checkbox(value, "");
    ui.end_row();
}

fn combo(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str]) {
    combo_help(ui, label, value, options, "");
}

fn combo_help(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str], help: &str) {
    let response = ui.label(label);
    if !help.is_empty() {
        response.on_hover_text(help);
    }
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
}

fn combo_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[&str],
    help: &str,
) {
    ui.add_enabled(enabled, egui::Label::new(label))
        .on_hover_text(help);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
