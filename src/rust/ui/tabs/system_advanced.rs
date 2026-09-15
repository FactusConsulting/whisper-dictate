//! System tab sections that are hidden in Simple settings mode. Split out of
//! `system.rs` (which had grown to ~350 lines across two blanket
//! `mode == Advanced` blocks) so each section can be gated row-by-row via
//! [`setting_visible`] instead: a schema `advanced` flag flip on any of these
//! keys now correctly shows/hides just that row, rather than needing the
//! blanket block to be edited too.
//!
//! The Maintenance cluster (Reload config / Doctor / Run benchmark / Config
//! file, plus the benchmark-results and corpus-record tools) is the one
//! exception: those are ACTIONS, not settings, so there is no schema key to
//! gate them by — they stay gated directly on `mode`, matching the feature
//! spec ("System: only ui_language, ui_theme" in Simple).

use super::system::{default_metrics_jsonl_path, text_scale_stepper};
use super::*;
use egui_material_icons::icons;

impl WhisperDictateApp {
    /// Maintenance actions + the benchmark-results and corpus-record tools.
    /// Advanced-only (no schema key backs these — see module docs).
    pub(in crate::ui) fn system_maintenance_section(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        mode: SettingsMode,
    ) {
        if mode != SettingsMode::Advanced {
            return;
        }
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemMaintenance),
            palette,
        );
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            // Reload is blocked while a background diagnostic/benchmark task is
            // active so config materialisation cannot race that task.
            let idle = self.background_task.is_none();
            let benchmark_idle =
                idle && self.runtime_state == crate::runtime::RuntimeState::Stopped;
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(icon_text(
                        icons::ICON_REFRESH.codepoint,
                        ui_text(&self.settings.ui_language, UiTextKey::ReloadConfig),
                    )),
                )
                .on_hover_text("Reload the config file from disk.")
                .clicked()
            {
                self.reload_settings();
            }
            if ui
                .button(icon_text(
                    icons::ICON_HEALTH_AND_SAFETY.codepoint,
                    ui_text(&self.settings.ui_language, UiTextKey::Doctor),
                ))
                .on_hover_text("Run environment diagnostics and write the result to the log.")
                .clicked()
            {
                self.run_doctor();
            }
            // Run benchmark: a slow background task (loads the model + runs the
            // whole corpus), so it shares the `idle` guard with Reload —
            // it must never freeze the UI and must not race another task. Output
            // (per-item JSONL + the `[benchmark]` summary line) lands in the log.
            if ui
                .add_enabled(
                    benchmark_idle,
                    egui::Button::new(icon_text(
                        icons::ICON_SPEED.codepoint,
                        ui_text(&self.settings.ui_language, UiTextKey::RunBenchmark),
                    )),
                )
                .on_hover_text(ui_text(
                    &self.settings.ui_language,
                    UiTextKey::RunBenchmarkHelp,
                ))
                .clicked()
            {
                self.run_benchmark();
            }
            if ui
                .button(icon_text(
                    icons::ICON_INFO.codepoint,
                    ui_text(&self.settings.ui_language, UiTextKey::ConfigFile),
                ))
                // Hover EXPLAINS the action, then shows the actual path so the
                // user knows both what the button does and where it lands.
                .on_hover_text(format!(
                    "Opens the folder containing config.json.\n{}",
                    self.config_path
                ))
                .clicked()
            {
                self.open_config_folder();
            }
        });
        // Discoverable help for the maintenance cluster: a `?` badge toggles a
        // wrapped explanation of every action, mirroring the settings-grid rows.
        const MAINTENANCE_HELP: &str = "Reload config: re-read config.json from disk (blocked while another background task runs). \
            Doctor: run environment diagnostics and write the result to the log. \
            Run benchmark: run the golden corpus through the configured backend and write the results + summary to the log (blocked while another task runs). \
            Config file: open the folder containing config.json.";
        let show_maintenance_help = ui
            .horizontal(|ui| help_toggle_badge(ui, "system_maintenance", MAINTENANCE_HELP))
            .inner;
        inline_help(ui, show_maintenance_help, MAINTENANCE_HELP);

        // The digestible "Run benchmark" results: a coloured headline + a
        // worst-WER-first table, shown below the button once a run completes. The
        // raw JSONL stays in the runtime log; this is the at-a-glance view.
        self.benchmark_results_section(ui, palette);

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Record corpus audio: read a corpus item's reference text aloud and
        // record it straight to the benchmark audio dir, so the golden corpus is
        // self-service. Sits next to "Run benchmark" since they share the corpus.
        self.corpus_record_section(ui, palette);

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
    }

    /// Display: dictation-view log mode (`ui_log_view`) + UI text scale
    /// (`ui_text_scale`). Both are non-schema UI prefs — `setting_visible`
    /// returns false for them in Simple the same way it does for any key with
    /// no schema/allow-list entry.
    pub(in crate::ui) fn system_display_section(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        mode: SettingsMode,
    ) {
        let show_log_view = setting_visible(mode, "ui_log_view");
        let show_text_scale = setting_visible(mode, "ui_text_scale");
        if !show_log_view && !show_text_scale {
            return;
        }
        ui.add_space(10.0);
        if show_log_view {
            ui.horizontal_wrapped(|ui| {
                section_label(
                    ui,
                    ui_text(&self.settings.ui_language, UiTextKey::SystemDisplay),
                    palette,
                );
                section_label(
                    ui,
                    ui_text(&self.settings.ui_language, UiTextKey::DictationView),
                    palette,
                );
                self.log_mode_selector(ui, palette);
            });
        }
        if show_text_scale {
            ui.add_space(12.0);
            settings_grid("system_appearance_settings").show(ui, |ui| {
                text_scale_stepper(
                    ui,
                    "UI text scale",
                    &mut self.settings.ui_text_scale,
                    "Scale all text in this settings UI. Use the −/+ buttons to step by 0.05 (clamped to 0.85–1.6). 1.0 is default, 1.15 is larger, 1.3 suits high-DPI displays.",
                );
            });
        }
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
    }

    /// Feedback: `feedback_sounds` only.
    pub(in crate::ui) fn system_feedback_section(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        mode: SettingsMode,
    ) {
        if !setting_visible(mode, "feedback_sounds") {
            return;
        }
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemFeedback),
            palette,
        );
        ui.add_space(6.0);
        settings_grid("system_feedback_settings").show(ui, |ui| {
            checkbox_help(
                ui,
                "Feedback sounds",
                &mut self.settings.feedback_sounds,
                "Play a short audio cue when recording starts and stops. Useful for headless/autostart usage where the console is hidden (Terminal=false).",
            );
        });
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
    }

    /// Updates: `update_check`, `update_check_interval_minutes`,
    /// `update_include_prereleases`.
    pub(in crate::ui) fn system_updates_section(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        mode: SettingsMode,
    ) {
        if !setting_visible(mode, "update_check") {
            return;
        }
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemUpdates),
            palette,
        );
        ui.add_space(6.0);
        settings_grid("system_updates_settings").show(ui, |ui| {
            let language = self.settings.ui_language.clone();
            checkbox_help(
                ui,
                ui_text(&language, UiTextKey::UpdateCheck),
                &mut self.settings.update_check,
                ui_text(&language, UiTextKey::UpdateCheckHelp),
            );
            // Interval is only meaningful while the check is enabled.
            if setting_visible(mode, "update_check_interval_minutes") {
                numeric_enabled(
                    ui,
                    &language,
                    self.settings.update_check,
                    "update_check_interval_minutes",
                    ui_text(&language, UiTextKey::UpdateCheckInterval),
                    &mut self.settings.update_check_interval_minutes,
                    ui_text(&language, UiTextKey::UpdateCheckIntervalHelp),
                );
            }
            // Opt-in to release-candidate notifications. Only meaningful while the
            // check is enabled, so it greys out alongside the interval field.
            if setting_visible(mode, "update_include_prereleases") {
                checkbox_enabled(
                    ui,
                    self.settings.update_check,
                    ui_text(&language, UiTextKey::UpdateIncludePrereleases),
                    &mut self.settings.update_include_prereleases,
                    ui_text(&language, UiTextKey::UpdateIncludePrereleasesHelp),
                );
            }
        });
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
    }

    /// Diagnostics: `log_level`. Moved here from the Output tab because the
    /// debug level is an app-wide concern, not part of the speech → text →
    /// output pipeline.
    pub(in crate::ui) fn system_diagnostics_section(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        mode: SettingsMode,
    ) {
        if !setting_visible(mode, "log_level") {
            return;
        }
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::Diagnostics),
            palette,
        );
        ui.add_space(6.0);
        settings_grid("system_diagnostics_settings").show(ui, |ui| {
            self.diagnostics_combo(ui);
        });
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
    }

    /// Integration: `json_output` (the `inject_json` field), `metrics_jsonl`,
    /// `local_only`.
    pub(in crate::ui) fn system_integration_section(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        mode: SettingsMode,
    ) {
        let show_json = setting_visible(mode, "json_output");
        let show_metrics = setting_visible(mode, "metrics_jsonl");
        let show_local_only = setting_visible(mode, "local_only");
        if !show_json && !show_metrics && !show_local_only {
            return;
        }
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemIntegration),
            palette,
        );
        ui.add_space(6.0);
        let metrics_path_hint = default_metrics_jsonl_path(&self.config_path);
        settings_grid("system_integration_settings").show(ui, |ui| {
            if show_json {
                checkbox_help(
                    ui,
                    "JSON stdout",
                    &mut self.settings.inject_json,
                    "Emit structured JSON events to stdout in addition to normal logs. This also gates the Metrics JSONL file — metrics are only written while this is enabled.",
                );
            }
            if show_metrics {
                let metrics_jsonl_before = self.settings.metrics_jsonl.clone();
                text_help_hint(
                    ui,
                    "Metrics JSONL",
                    &mut self.settings.metrics_jsonl,
                    "Path for appending transcription metrics as JSONL. Metrics are only written while \"JSON stdout\" is enabled. An empty value keeps metrics-file output disabled.",
                    &metrics_path_hint,
                );
                let metrics_jsonl_after = self.settings.metrics_jsonl.clone();
                self.record_nullable_text_edit(
                    "metrics_jsonl",
                    &metrics_jsonl_before,
                    &metrics_jsonl_after,
                );
            }
            if show_local_only {
                checkbox_help(
                    ui,
                    "Local only",
                    &mut self.settings.local_only,
                    "Privacy lock: block network-backed STT and post-processing providers when enabled.",
                );
            }
        });
        if show_metrics {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                // Keep the suggested path display-only until the user explicitly
                // chooses it, so a persisted null remains an authoritative clear.
                if ui
                    .button(ui_text(
                        &self.settings.ui_language,
                        UiTextKey::UseDefaultPath,
                    ))
                    .on_hover_text(default_metrics_jsonl_path(&self.config_path))
                    .clicked()
                {
                    self.use_default_metrics_jsonl_path();
                }
                if ui.button("Preview metrics").clicked() {
                    self.preview_metrics();
                }
                if ui.button("Open metrics").clicked() {
                    self.open_metrics();
                }
            });
            if !self.metrics_preview.is_empty() {
                ui.label("Metrics preview");
                let response = ui.add(
                    egui::TextEdit::multiline(&mut self.metrics_preview)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(8)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
                // Same below-the-fold problem as the history preview: scroll the
                // freshly loaded preview into view on its first frame, then clear
                // the one-shot flag.
                if self.scroll_to_metrics_preview {
                    response.scroll_to_me(Some(egui::Align::Center));
                    self.scroll_to_metrics_preview = false;
                }
            }
        }
    }

    /// Native logger level. This writes the same `VOICEPI_LOG` value consumed
    /// by every debug/trace diagnostic in the Rust runtime.
    fn diagnostics_combo(&mut self, ui: &mut egui::Ui) {
        let label = ui_text(&self.settings.ui_language, UiTextKey::Diagnostics);
        let help = ui_text(&self.settings.ui_language, UiTextKey::DiagnosticsHelp);
        let show_help = label_with_help(ui, label, help);
        let current = diagnostics_level(&self.settings.log_level);
        let language = self.settings.ui_language.clone();
        let level_label = |level: DiagnosticsLevel| -> &'static str {
            ui_text(
                &language,
                match level {
                    DiagnosticsLevel::Off => UiTextKey::DiagnosticsOff,
                    DiagnosticsLevel::Basic => UiTextKey::DiagnosticsBasic,
                    DiagnosticsLevel::Verbose => UiTextKey::DiagnosticsVerbose,
                    DiagnosticsLevel::Trace => UiTextKey::DiagnosticsTrace,
                },
            )
        };
        let mut selected = current;
        egui::ComboBox::from_id_salt("diagnostics_level")
            .width(settings_short_control_width(ui))
            .selected_text(level_label(current))
            .show_ui(ui, |ui| {
                for level in DiagnosticsLevel::ALL {
                    ui.selectable_value(&mut selected, level, level_label(level));
                }
            });
        if selected != current {
            self.settings.log_level = apply_diagnostics_level(selected).to_owned();
        }
        ui.end_row();
        grid_help_row(ui, show_help, help);
    }

    /// Open the folder that contains the config file (not the file itself), so
    /// the user lands in a place where they can inspect/back up the JSON. Reuses
    /// the console-window-guarded `open_existing_path` helper.
    fn open_config_folder(&mut self) {
        // A relative VOICEPI_CONFIG like "config.json" has an EMPTY parent —
        // fall back to the current directory instead of failing exists().
        let folder = match std::path::Path::new(&self.config_path).parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        };
        // First run: the config directory may not exist until the first save —
        // create it so the button works from a fresh install too.
        let _ = std::fs::create_dir_all(&folder);
        match config::open_existing_path(&folder) {
            Ok(path) => self.settings_status = format!("Opened config folder: {}", path.display()),
            Err(err) => self.settings_status = format!("Open config folder failed: {err}"),
        }
    }
}
