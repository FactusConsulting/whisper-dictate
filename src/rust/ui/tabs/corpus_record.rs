//! The System tab's "Record corpus audio" cluster (rendering only).
//!
//! Split out of `tabs/system.rs` so that file stays under the module-size limit
//! and so the record-corpus feature's UI lives in one place. The picker/parsing
//! logic, localized strings and background-task wiring live in
//! `ui/corpus.rs`, `ui/corpus_record.rs` and the `ui/tasks.rs` record block;
//! this module only paints them.

use super::super::*;
use super::*;
use egui_material_icons::icons;

impl WhisperDictateApp {
    /// The "Record corpus audio" cluster: a picker of corpus items, the selected
    /// item's reference text (read aloud), and a Record button that launches the
    /// `--record-corpus-item` worker. Gated on the dictation runtime being stopped
    /// (recording must never disturb the managed runtime — they would fight over
    /// the microphone) and no background task running; a localized hint explains
    /// the runtime block. The terminal done/error event is shown inline.
    pub(in crate::ui) fn corpus_record_section(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        // Lazy-load the corpus the first time this tab renders so the picker is
        // populated without a manual refresh.
        self.ensure_corpus_loaded(false);
        let language = self.settings.ui_language.clone();

        section_label(
            ui,
            corpus_record_text(&language, CorpusRecordText::SectionLabel),
            palette,
        );
        // Discoverable help: a `?` badge toggles a wrapped explanation of the
        // whole record-corpus cluster, mirroring the maintenance cluster above.
        let section_help = corpus_record_text(&language, CorpusRecordText::SectionHelp);
        let show_section_help = ui
            .horizontal(|ui| help_toggle_badge(ui, "system_corpus_record", section_help))
            .inner;
        inline_help(ui, show_section_help, section_help);

        // Always-visible purpose line so the feature is unmistakable: these clips
        // are the benchmark's golden reference audio, NOT dictionary entries.
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(corpus_record_text(
                &language,
                CorpusRecordText::PurposeIntro,
            ))
            .color(palette.text_muted),
        );
        ui.add_space(6.0);

        if self.corpus_items.is_empty() {
            ui.label(
                egui::RichText::new(corpus_record_text(&language, CorpusRecordText::NoItems))
                    .color(palette.text_muted),
            );
            return;
        }

        // A batch run takes over the cluster: while it walks the corpus item by
        // item the picker/Record controls are replaced by a progress panel with a
        // Stop button. The single-item path is restored as soon as it ends.
        if self.corpus_batch_active() {
            self.corpus_batch_panel(ui, &language, palette);
            return;
        }

        let recording = self.background_task_label == Some(RECORD_CORPUS_ITEM_LABEL);

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            ui.label(corpus_record_text(&language, CorpusRecordText::PickerLabel));
            // The combo shows `id — preview` and a ✓ for already-recorded items so
            // the user can see at a glance which still need a recording.
            // `corpus_recorded_ids` is populated once per load — no per-frame I/O.
            let recorded_ids = &self.corpus_recorded_ids;
            let selected_label = self
                .corpus_selected_id
                .as_ref()
                .and_then(|id| self.corpus_items.iter().find(|item| &item.id == id))
                .map(|item| combo_entry_label(item, recorded_ids, &language))
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("corpus_record_item")
                .selected_text(selected_label)
                .width(360.0)
                .show_ui(ui, |ui| {
                    for item in &self.corpus_items {
                        let label = combo_entry_label(item, recorded_ids, &language);
                        ui.selectable_value(
                            &mut self.corpus_selected_id,
                            Some(item.id.clone()),
                            label,
                        );
                    }
                });

            // Record is blocked while the runtime is running (it owns the mic) or a
            // background task is in flight. `can_record_corpus_item` is the single
            // source of truth, mirrored by the test.
            let enabled = self.can_record_corpus_item();
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(icon_text(
                        icons::ICON_FIBER_MANUAL_RECORD.codepoint,
                        corpus_record_text(&language, CorpusRecordText::RecordButton),
                    )),
                )
                .on_hover_text(corpus_record_text(
                    &language,
                    CorpusRecordText::RecordButtonHelp,
                ))
                .clicked()
            {
                self.run_record_corpus_item();
            }
        });

        // Batch controls: walk every missing item, or re-record everything, in one
        // sequence. Each clip still uses the single-item worker (chained in the UI
        // on every done-event); these only seed the queue. Gated like the single
        // Record button via `can_start_corpus_batch`.
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let can_start = self.can_start_corpus_batch();
            if ui
                .add_enabled(
                    can_start,
                    egui::Button::new(icon_text(
                        icons::ICON_PLAYLIST_ADD_CHECK.codepoint,
                        corpus_record_text(&language, CorpusRecordText::RecordAllMissing),
                    )),
                )
                .clicked()
            {
                self.start_corpus_batch(BatchScope::AllMissing);
            }
            if ui
                .add_enabled(
                    can_start,
                    egui::Button::new(icon_text(
                        icons::ICON_REPLAY.codepoint,
                        corpus_record_text(&language, CorpusRecordText::RecordAll),
                    )),
                )
                .clicked()
            {
                self.start_corpus_batch(BatchScope::All);
            }
        });

        // When every item already has a recording, "Record all missing" would do
        // nothing — say so (computed from the cached recorded-ids set; no I/O).
        let all_recorded = self
            .corpus_items
            .iter()
            .all(|item| self.corpus_recorded_ids.contains(&item.id));
        if all_recorded {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(corpus_record_text(
                    &language,
                    CorpusRecordText::BatchNothingToRecord,
                ))
                .color(palette.text_muted),
            );
        }

        // Clarity one-liner next to the Record actions: re-recording overwrites
        // only that item's own WAV; the dictionary is never touched.
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(corpus_record_text(
                &language,
                CorpusRecordText::OverwriteNote,
            ))
            .color(palette.text_muted),
        );

        // The runtime-running block has a dedicated localized hint so the greyed
        // button is never a dead end.
        if self.runtime_state != RuntimeState::Stopped {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(corpus_record_text(
                    &language,
                    CorpusRecordText::StopRuntimeHint,
                ))
                .color(palette.warn_text),
            );
        }

        // The reference text to read aloud, shown for the selected item so the
        // user can read while recording (the recording flow itself is fixed-
        // duration; the text appears here, not in a transient worker line).
        if let Some(item) = self
            .corpus_selected_id
            .as_ref()
            .and_then(|id| self.corpus_items.iter().find(|item| &item.id == id))
        {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(corpus_record_text(
                    &language,
                    CorpusRecordText::ReadAloudPrompt,
                ))
                .color(palette.text_muted),
            );
            ui.label(egui::RichText::new(&item.text).italics());
        }

        // Inline status: a spinner while recording, then the saved/failed result.
        self.corpus_record_status(ui, &language, palette, recording);
    }

    /// Render the inline record status: a "Recording…" spinner while the worker
    /// runs, then the saved confirmation (path + duration) or the error message
    /// once it finishes. Reads `corpus_record_result` (transient UI state).
    fn corpus_record_status(
        &self,
        ui: &mut egui::Ui,
        language: &str,
        palette: UiPalette,
        recording: bool,
    ) {
        if recording {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(
                    egui::RichText::new(corpus_record_text(language, CorpusRecordText::Recording))
                        .color(palette.text_muted),
                );
            });
            return;
        }
        let Some(result) = self.corpus_record_result.as_ref() else {
            return;
        };
        ui.add_space(6.0);
        match result {
            Ok(CorpusRecordOutcome::Saved {
                path,
                seconds_recorded,
                ..
            }) => {
                let saved = corpus_record_text(language, CorpusRecordText::Saved);
                ui.label(
                    icon_text(
                        icons::ICON_CHECK_CIRCLE.codepoint,
                        format!("{saved}: {path} ({seconds_recorded:.1}s)"),
                    )
                    .color(palette.ok_text),
                );
            }
            Ok(CorpusRecordOutcome::Failed { error }) => {
                ui.label(icon_text(icons::ICON_ERROR.codepoint, error).color(palette.warn_text));
            }
            Err(message) => {
                ui.label(
                    icon_text(icons::ICON_WARNING.codepoint, message).color(palette.warn_text),
                );
            }
        }
    }

    /// Render the active-batch panel: an "Item k of N · recorded c" progress line,
    /// the current item's reference text to read aloud, a recording spinner while
    /// the clip's worker runs, and a Stop button. Replaces the picker/Record
    /// controls for the duration of the run. Reads the transient `corpus_batch`
    /// cursor; the actual advance/launch is driven by `poll_corpus_batch`.
    fn corpus_batch_panel(&mut self, ui: &mut egui::Ui, language: &str, palette: UiPalette) {
        let Some(batch) = self.corpus_batch.as_ref() else {
            return;
        };
        let progress =
            batch_progress_line(language, batch.position(), batch.total(), batch.completed());
        // Resolve the current item's index in corpus_items (by id) so we can
        // borrow its text as &str after the mutable UI section — no per-frame
        // String clone.
        let current_item_idx = batch
            .current()
            .and_then(|id| self.corpus_items.iter().position(|item| item.id == id));
        // The immutable borrows of corpus_batch and corpus_items end here.

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(progress).strong());
            if ui
                .button(icon_text(
                    icons::ICON_STOP.codepoint,
                    corpus_record_text(language, CorpusRecordText::StopBatch),
                ))
                .clicked()
            {
                self.stop_corpus_batch();
            }
        });

        if let Some(idx) = current_item_idx {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(corpus_record_text(
                    language,
                    CorpusRecordText::BatchReadAloudPrompt,
                ))
                .color(palette.text_muted),
            );
            ui.label(egui::RichText::new(&self.corpus_items[idx].text).italics());
        }

        // Spinner while the current clip's worker is recording (between clips, the
        // brief gap shows no spinner — the next item's text is already visible).
        if self.background_task_label == Some(RECORD_CORPUS_ITEM_LABEL) {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(
                    egui::RichText::new(corpus_record_text(language, CorpusRecordText::Recording))
                        .color(palette.text_muted),
                );
            });
        }
    }
}
