use anyhow::Result;
use eframe::egui;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::config::{self, AppSettings};
use crate::runtime::{
    default_worker_command, doctor_command, install_command, run_capture, RuntimeEvent,
    RuntimeState, RuntimeSupervisor, WorkerCommand,
};

pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([920.0, 680.0]),
        ..Default::default()
    };

    eframe::run_native(
        "whisper-dictate",
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
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.runtime_log)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(24)
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
        egui::Grid::new("core_settings")
            .num_columns(2)
            .show(ui, |ui| {
                combo(
                    ui,
                    "STT backend",
                    &mut self.settings.stt_backend,
                    &["whisper", "parakeet", "openai"],
                );
                text(ui, "Whisper model", &mut self.settings.model);
                text(ui, "External STT model", &mut self.settings.stt_model);
                text(ui, "Parakeet model", &mut self.settings.parakeet_model);
                combo(
                    ui,
                    "Device",
                    &mut self.settings.device,
                    &["auto", "cuda", "cpu"],
                );
                combo(
                    ui,
                    "Compute type",
                    &mut self.settings.compute_type,
                    &["", "int8_float16", "float16", "bfloat16", "float32", "int8"],
                );
                combo(
                    ui,
                    "Language",
                    &mut self.settings.lang,
                    &["", "da", "en", "de", "fr", "sv", "nb", "nl", "es", "it"],
                );
                text(ui, "STT API URL", &mut self.settings.stt_base_url);
                text(ui, "STT timeout ms", &mut self.settings.stt_timeout_ms);
                text(ui, "Hotkey", &mut self.settings.key);
                text(ui, "Quit count", &mut self.settings.quit_count);
                text(ui, "Quit window ms", &mut self.settings.quit_window_ms);
            });
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
}

fn text(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).desired_width(360.0));
    ui.end_row();
}

fn checkbox(ui: &mut egui::Ui, label: &str, value: &mut bool) {
    ui.label(label);
    ui.checkbox(value, "");
    ui.end_row();
}

fn combo(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str]) {
    ui.label(label);
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
