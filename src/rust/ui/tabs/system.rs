//! The System tab: app-level maintenance and settings that are not part of the
//! speech → text → output pipeline. It collects the runtime maintenance actions
//! that used to live in the sidebar (Reload config / Doctor / Install-Repair +
//! the config-file shortcut) and the appearance/display/feedback/integration
//! settings that used to crowd the Output tab.
//!
//! Keeping these here lets the sidebar stay a slim navigator and the Output tab
//! stay focused on how dictated speech is turned into injected text.

use super::super::*;
use super::*;
use egui_material_icons::icons;

impl WhisperDictateApp {
    pub(in crate::ui) fn system_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading(ui_text(&self.settings.ui_language, UiTextKey::System));
        ui.add_space(8.0);

        // --- Maintenance: the runtime actions that used to live in the sidebar.
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemMaintenance),
            palette,
        );
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            // Reload config + Install/Repair share the "another background task
            // is running" guard. For Install that matches its old sidebar logic;
            // Reload was unguarded in the sidebar and deliberately GAINS the
            // guard here (reloading mid-install/doctor would race the task).
            let idle = self.background_task.is_none();
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
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(icon_text(
                        icons::ICON_BUILD.codepoint,
                        ui_text(&self.settings.ui_language, UiTextKey::InstallRepair),
                    )),
                )
                .on_hover_text("Install or repair the local runtime environment.")
                .clicked()
            {
                self.run_install();
            }
            // Run benchmark: a slow background task (loads the model + runs the
            // whole corpus), so it shares the `idle` guard with Install/Reload —
            // it must never freeze the UI and must not race another task. Output
            // (per-item JSONL + the `[benchmark]` summary line) lands in the log.
            if ui
                .add_enabled(
                    idle,
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
            // Issue #328: re-open the onboarding wizard on demand. Enabled
            // regardless of `idle` — the wizard just paints a modal, no
            // background task is spawned.
            if ui
                .button(icon_text(
                    icons::ICON_PLAY_ARROW.codepoint,
                    "Run setup again",
                ))
                .on_hover_text(
                    "Re-open the first-run onboarding wizard (microphone, hotkey, \
                     backend, permissions, test recording).",
                )
                .clicked()
            {
                self.reopen_onboarding_wizard();
            }
        });
        // Discoverable help for the maintenance cluster: a `?` badge toggles a
        // wrapped explanation of every action, mirroring the settings-grid rows.
        const MAINTENANCE_HELP: &str = "Reload config: re-read config.json from disk (blocked while another background task runs). \
            Doctor: run environment diagnostics and write the result to the log. \
            Install/Repair: install or repair the local runtime environment (blocked while another task runs). \
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

        // --- Appearance + Display: chrome that used to sit at the top of Output.
        ui.horizontal_wrapped(|ui| {
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::SystemAppearance),
                palette,
            );
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::UiTheme),
                palette,
            );
            let ui_language = self.settings.ui_language.clone();
            theme_toggle(ui, &mut self.settings.ui_theme, palette, &ui_language);
            ui.add_space(12.0);
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::UiLanguage),
                palette,
            );
            // A dropdown (rather than the old two-button toggle) so more UI
            // languages can be added later without crowding the row. Writes the
            // same raw "en"/"da" config values.
            let language = self.settings.ui_language.clone();
            let options = [
                ("en", ui_text(&language, UiTextKey::English)),
                ("da", ui_text(&language, UiTextKey::Danish)),
            ];
            let selected = options
                .iter()
                .find(|(raw, _)| *raw == self.settings.ui_language)
                .map(|(_, display)| *display)
                .unwrap_or_else(|| ui_text(&language, UiTextKey::English));
            egui::ComboBox::from_id_salt("ui_language_select")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for (raw, display) in options {
                        ui.selectable_value(
                            &mut self.settings.ui_language,
                            raw.to_owned(),
                            display,
                        );
                    }
                });
        });
        ui.add_space(10.0);
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
        ui.add_space(12.0);
        settings_grid("system_appearance_settings").show(ui, |ui| {
            text_scale_stepper(
                ui,
                "UI text scale",
                &mut self.settings.ui_text_scale,
                "Scale all text in this settings UI. Use the −/+ buttons to step by 0.05 (clamped to 0.85–1.6). 1.0 is default, 1.15 is larger, 1.3 suits high-DPI displays.",
            );
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Recording overlay (Issue #320): the small always-on-top window
        // with the live audio meter. Kept in its own section so the settings
        // stay grouped — the toggle controls visibility, the position picker
        // controls anchoring, and "Also show while idle" turns it into a
        // permanent meter for device-setup mode.
        section_label(ui, "Recording overlay", palette);
        ui.add_space(6.0);
        settings_grid("system_overlay_settings").show(ui, |ui| {
            checkbox_help(
                ui,
                "Show recording overlay",
                &mut self.settings.overlay_enabled,
                "Show a small always-on-top window with a live audio level meter \
                 while dictating, so you can see the mic is working without \
                 squinting at the tray icon.",
            );
            // Position picker: the four corners + a custom anchor that is
            // produced (and persisted) by dragging the overlay window. The
            // custom case is shown but not selectable from the picker — it
            // exists so the dropdown can DISPLAY the current state honestly.
            ui.label("Overlay position");
            let current_position =
                crate::ui::OverlayPosition::parse(&self.settings.overlay_position);
            let selected_label = match current_position {
                crate::ui::OverlayPosition::TopLeft => "Top-left",
                crate::ui::OverlayPosition::TopRight => "Top-right",
                crate::ui::OverlayPosition::BottomLeft => "Bottom-left",
                crate::ui::OverlayPosition::BottomRight => "Bottom-right",
                crate::ui::OverlayPosition::Custom { .. } => "Custom (dragged)",
            };
            egui::ComboBox::from_id_salt("overlay_position_select")
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    for (variant, label) in [
                        (crate::ui::OverlayPosition::TopLeft, "Top-left"),
                        (crate::ui::OverlayPosition::TopRight, "Top-right"),
                        (crate::ui::OverlayPosition::BottomLeft, "Bottom-left"),
                        (crate::ui::OverlayPosition::BottomRight, "Bottom-right"),
                    ] {
                        if ui
                            .selectable_label(current_position == variant, label)
                            .clicked()
                        {
                            self.settings.overlay_position = variant.to_storage_string();
                        }
                    }
                });
            ui.end_row();
            checkbox_help(
                ui,
                "Also show while idle",
                &mut self.settings.overlay_show_on_idle,
                "Keep the overlay on screen even when not dictating — useful as a \
                 permanent meter while configuring microphones, but distracting \
                 in everyday use.",
            );
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Feedback: the audio/notification cues moved out of Output.
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
            checkbox_help(
                ui,
                "Feedback notifications",
                &mut self.settings.feedback_notify,
                "Show a desktop notification when an error occurs (model load failure, audio capture lost, injection failure). Useful for headless/autostart usage where the console is hidden.",
            );
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Updates: the in-app "update available" check.
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
            numeric_enabled(
                ui,
                &language,
                self.settings.update_check,
                "update_check_interval_minutes",
                ui_text(&language, UiTextKey::UpdateCheckInterval),
                &mut self.settings.update_check_interval_minutes,
                ui_text(&language, UiTextKey::UpdateCheckIntervalHelp),
            );
            // Opt-in to release-candidate notifications. Only meaningful while the
            // check is enabled, so it greys out alongside the interval field.
            checkbox_enabled(
                ui,
                self.settings.update_check,
                ui_text(&language, UiTextKey::UpdateIncludePrereleases),
                &mut self.settings.update_include_prereleases,
                ui_text(&language, UiTextKey::UpdateIncludePrereleasesHelp),
            );
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Diagnostics: app-level worker verbosity. Moved here from the Output
        // tab because the debug level is an app-wide concern, not part of the
        // speech → text → output pipeline. Sits next to Integration since both are
        // about what the worker emits for inspection.
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

        // --- Integration: machine-readable outputs moved out of Output.
        section_label(
            ui,
            ui_text(&self.settings.ui_language, UiTextKey::SystemIntegration),
            palette,
        );
        ui.add_space(6.0);
        settings_grid("system_integration_settings").show(ui, |ui| {
            checkbox_help(
                ui,
                "JSON stdout",
                &mut self.settings.inject_json,
                "Emit structured JSON events to stdout in addition to normal logs. This also gates the Metrics JSONL file — metrics are only written while this is enabled.",
            );
            text_help(
                ui,
                "Metrics JSONL",
                &mut self.settings.metrics_jsonl,
                "Path for appending transcription metrics as JSONL. Metrics are only written while \"JSON stdout\" is enabled, so a prefilled path stays inert until you opt in.",
            );
            checkbox_help(
                ui,
                "Local only",
                &mut self.settings.local_only,
                "Privacy lock: block network-backed STT and post-processing providers when enabled.",
            );
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // The field is already prefilled at load (and after "Reload config")
            // with the suggested path next to config.json. This button restores
            // that default after the user has edited the field. Metrics are still
            // only written while "JSON stdout" is enabled, so a prefilled path
            // stays inert until the user opts in.
            if ui
                .button(ui_text(
                    &self.settings.ui_language,
                    UiTextKey::UseDefaultPath,
                ))
                .on_hover_text(default_metrics_jsonl_path(&self.config_path))
                .clicked()
            {
                self.settings.metrics_jsonl = default_metrics_jsonl_path(&self.config_path);
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

    /// One "Diagnostics" dropdown standing in for the two raw debug toggles.
    ///
    /// The persisted `debug` / `stt_debug` bools (and their env vars + the
    /// worker) are unchanged — this is a pure UI affordance over them via
    /// [`diagnostics_level`] / [`apply_diagnostics_level`]. The level is read
    /// from the current bools each frame; on change both bools are written so
    /// the dirty-dot and Save behave exactly as the old checkboxes did. A SHORT
    /// width is used since the options (Off/Basic/Verbose) are tiny enum tokens.
    fn diagnostics_combo(&mut self, ui: &mut egui::Ui) {
        let label = ui_text(&self.settings.ui_language, UiTextKey::Diagnostics);
        let help = ui_text(&self.settings.ui_language, UiTextKey::DiagnosticsHelp);
        let show_help = label_with_help(ui, label, help);
        let current = diagnostics_level(
            self.settings.debug,
            self.settings.stt_debug,
            self.settings.trace,
        );
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
            let (debug, stt_debug, trace) = apply_diagnostics_level(selected);
            self.settings.debug = debug;
            self.settings.stt_debug = stt_debug;
            self.settings.trace = trace;
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

/// Suggested metrics path: `metrics.jsonl` next to the config file (the
/// app-data folder the user already knows). A relative config path with an
/// empty parent suggests a bare `metrics.jsonl` in the working directory.
/// Pure so it is unit-testable.
pub(in crate::ui) fn default_metrics_jsonl_path(config_path: &str) -> String {
    match std::path::Path::new(config_path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.join("metrics.jsonl").to_string_lossy().into_owned()
        }
        _ => "metrics.jsonl".to_owned(),
    }
}

/// A compact UI-text-scale row: short text input flanked by "−"/"+" stepper
/// buttons that nudge the value by 0.05 within the theme's clamp range.
/// Placed here (tabs/system.rs — its only consumer) so it falls under the
/// `src/rust/ui/tabs/**` Sonar coverage exclusion for render code. The pure
/// `step_text_scale` logic stays in `text_scale.rs` where it is unit-tested.
pub(in crate::ui) fn text_scale_stepper(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    help: &str,
) {
    const STEP: f32 = 0.05;
    let show_help = label_with_help(ui, label, help);
    ui.horizontal(|ui| {
        if ui.small_button("−").on_hover_text("Smaller text").clicked() {
            *value = step_text_scale(value, -STEP);
        }
        ui.add(egui::TextEdit::singleline(value).desired_width(60.0));
        if ui.small_button("+").on_hover_text("Larger text").clicked() {
            *value = step_text_scale(value, STEP);
        }
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}
