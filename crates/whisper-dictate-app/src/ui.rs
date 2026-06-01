use anyhow::Result;
use eframe::egui;

use crate::config;
use crate::runtime::{default_worker_command, RuntimeEvent, RuntimeState, RuntimeSupervisor};

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

#[derive(Debug)]
struct WhisperDictateApp {
    selected_tab: Tab,
    runtime_state: RuntimeState,
    runtime_log: String,
    config_path: String,
    supervisor: RuntimeSupervisor,
}

impl Default for WhisperDictateApp {
    fn default() -> Self {
        Self {
            selected_tab: Tab::Runtime,
            runtime_state: RuntimeState::Stopped,
            runtime_log: "Rust UI ready. Start launches the Python dictation worker directly."
                .to_owned(),
            config_path: config::config_path().display().to_string(),
            supervisor: RuntimeSupervisor::new(),
        }
    }
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
            Tab::Core => self.placeholder_tab(ui, "Core settings"),
            Tab::Quality => self.placeholder_tab(ui, "Quality settings"),
            Tab::Dictionary => self.placeholder_tab(ui, "Dictionary"),
            Tab::Output => self.placeholder_tab(ui, "Output settings"),
            Tab::Profiles => self.placeholder_tab(ui, "Profiles"),
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
            ui.separator();
            ui.label(format!("Status: {}", self.runtime_state.label()));
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

    fn placeholder_tab(&self, ui: &mut egui::Ui, title: &str) {
        ui.heading(title);
        ui.label("This section is scaffolded for the Rust UI migration.");
        ui.label(format!("Config: {}", self.config_path));
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
}
