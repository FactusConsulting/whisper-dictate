use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn output_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading("Output");
        ui.add_space(8.0);
        self.session_panel(ui, palette);
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
        settings_grid("output_settings")
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
                text_help(
                    ui,
                    "Command hook",
                    &mut self.settings.command_hook,
                    "Optional command run after accepted utterances for advanced automation.",
                );
                text_help_short(
                    ui,
                    "Command hook timeout ms",
                    &mut self.settings.command_hook_timeout_ms,
                    "Maximum time the command hook may run before it is treated as timed out.",
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
                self.diagnostics_combo(ui);
            });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Preview history").clicked() {
                self.preview_history();
            }
            if ui.button("Open history").clicked() {
                self.open_history();
            }
        });
        if !self.history_preview.is_empty() {
            ui.label("History preview");
            let response = ui.add(
                egui::TextEdit::multiline(&mut self.history_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
            // The settings body lives in a vertical ScrollArea, so a freshly
            // loaded preview renders below the fold and the click reads as "did
            // nothing". Scroll the preview into view on the frame it loads, then
            // clear the one-shot flag (mirrors `runtime_log_scroll_to_bottom`).
            if self.scroll_to_history_preview {
                response.scroll_to_me(Some(egui::Align::Center));
                self.scroll_to_history_preview = false;
            }
        }
    }

    /// One "Diagnostics" dropdown standing in for the two raw debug toggles.
    ///
    /// The persisted `debug` / `stt_debug` bools (and their env vars + the
    /// worker) are unchanged — this is a pure UI affordance over them via
    /// [`diagnostics_level`] / [`apply_diagnostics_level`]. The level is read
    /// from the current bools each frame; on change both bools are written so
    /// the dirty-dot and Save behave exactly as the old checkboxes did.
    fn diagnostics_combo(&mut self, ui: &mut egui::Ui) {
        let label = ui_text(&self.settings.ui_language, UiTextKey::Diagnostics);
        const HELP: &str = "How much diagnostic output the worker prints. \
            Basic = the effective-configuration dump at startup. \
            Verbose = Basic plus per-utterance speech-to-text and dictionary-helper detail. \
            Set the Dictation view to \"Debug\" to see the raw lines in the log.";
        let show_help = label_with_help(ui, label, HELP);
        let current = diagnostics_level(self.settings.debug, self.settings.stt_debug);
        let language = self.settings.ui_language.clone();
        let level_label = |level: DiagnosticsLevel| -> &'static str {
            ui_text(
                &language,
                match level {
                    DiagnosticsLevel::Off => UiTextKey::DiagnosticsOff,
                    DiagnosticsLevel::Basic => UiTextKey::DiagnosticsBasic,
                    DiagnosticsLevel::Verbose => UiTextKey::DiagnosticsVerbose,
                },
            )
        };
        let mut selected = current;
        egui::ComboBox::from_id_salt("diagnostics_level")
            .width(settings_control_width(ui))
            .selected_text(level_label(current))
            .show_ui(ui, |ui| {
                for level in DiagnosticsLevel::ALL {
                    ui.selectable_value(&mut selected, level, level_label(level));
                }
            });
        if selected != current {
            let (debug, stt_debug) = apply_diagnostics_level(selected);
            self.settings.debug = debug;
            self.settings.stt_debug = stt_debug;
        }
        ui.end_row();
        grid_help_row(ui, show_help, HELP);
    }
}
