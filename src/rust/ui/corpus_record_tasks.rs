//! Background-task wiring for "record a corpus item from the app".
//!
//! Kept in its own module (not in `tasks.rs`) so that file stays under the
//! module-size limit and so the corpus-record feature's logic — the lazy corpus
//! load, the gated launch, the can-record predicate and the finished-run handler
//! — lives next to its parser/strings (`corpus.rs` / `corpus_record.rs`). The
//! `poll_background_task` routing in `tasks.rs` dispatches the
//! `RECORD_CORPUS_ITEM_LABEL` result to `apply_corpus_record` here.

use super::*;
use crate::config;
use crate::runtime::{record_corpus_item_command, resource_app_root};
use std::path::PathBuf;

/// Background-task label for the worker's `--record-corpus-item` run. Routed in
/// `poll_background_task` to `apply_corpus_record`, which parses the terminal
/// done/error event into the inline System-tab confirmation.
pub(in crate::ui) const RECORD_CORPUS_ITEM_LABEL: &str = "record corpus item";

/// The per-user appdata dir (`%APPDATA%/WhisperDictate` or the XDG equivalent),
/// where corpus recordings live under `benchmark/audio`. Shared by the corpus
/// load (already-recorded markers) and the post-record refresh.
pub(in crate::ui) fn corpus_appdata_dir() -> PathBuf {
    config::platform_config_dir()
}

impl WhisperDictateApp {
    /// Load the golden-benchmark corpus from disk into `corpus_items` (once).
    ///
    /// Resolves `benchmark/corpus.json` the way the worker does (app-root, then
    /// appdata) and parses it into the picker's items. Idempotent via
    /// `corpus_loaded`; safe to call every frame the System tab renders. A
    /// missing/malformed manifest leaves `corpus_items` empty (the picker shows a
    /// "no items" placeholder) — never an error popup. `force` re-reads even when
    /// already loaded (used after a recording so the ✓ markers refresh).
    pub(in crate::ui) fn ensure_corpus_loaded(&mut self, force: bool) {
        if self.corpus_loaded && !force {
            return;
        }
        self.corpus_loaded = true;
        let app_root = resource_app_root();
        let appdata = corpus_appdata_dir();
        self.corpus_items = match resolve_corpus_path(&app_root, &appdata) {
            Some(path) => std::fs::read_to_string(&path)
                .map(|raw| parse_corpus(&raw))
                .unwrap_or_default(),
            None => Vec::new(),
        };
        // Default the selection to the first item if nothing valid is selected.
        let selection_valid = self
            .corpus_selected_id
            .as_ref()
            .is_some_and(|id| self.corpus_items.iter().any(|item| &item.id == id));
        if !selection_valid {
            self.corpus_selected_id = self.corpus_items.first().map(|item| item.id.clone());
        }
    }

    /// Launch the worker to record the selected corpus item's reference audio.
    ///
    /// Gated like the other heavy tools: requires the dictation runtime STOPPED
    /// (recording must never disturb the managed runtime — it would fight over the
    /// microphone) AND no other background task in flight. The shared
    /// `run_background_command` enforces the background-task half; the runtime
    /// half is checked here with a localized hint. Clears any previous result so
    /// the user sees the in-flight state, not a stale outcome.
    pub(in crate::ui) fn run_record_corpus_item(&mut self) {
        let Some(id) = self.corpus_selected_id.clone() else {
            return;
        };
        if self.runtime_state != RuntimeState::Stopped {
            let hint = corpus_record_text(
                &self.settings.ui_language,
                CorpusRecordText::StopRuntimeHint,
            );
            self.append_runtime_log(format!("[ui] record corpus item skipped: {hint}"));
            return;
        }
        self.corpus_record_result = None;
        self.run_background_command(RECORD_CORPUS_ITEM_LABEL, record_corpus_item_command(&id));
    }

    /// Whether the Record button should be enabled: an item is selected, the
    /// runtime is stopped, and no background task is running. Pure so it is
    /// unit-testable without egui.
    pub(in crate::ui) fn can_record_corpus_item(&self) -> bool {
        self.corpus_selected_id.is_some()
            && self.runtime_state == RuntimeState::Stopped
            && self.background_task.is_none()
    }

    /// Handle a finished `--record-corpus-item` run: parse the terminal
    /// done/error event into the inline System-tab confirmation
    /// (`corpus_record_result`) and log the outcome. A run failure (worker
    /// couldn't even start) is stored as an `Err`. On a successful save the
    /// corpus is re-read so the just-recorded item gains its ✓ marker.
    pub(in crate::ui) fn apply_corpus_record(&mut self, result: &BackgroundTaskResult) {
        if let Some(error) = &result.error {
            let message = format!("Could not record corpus item: {error}");
            self.append_runtime_log(format!("[ERROR] {message}"));
            self.corpus_record_result = Some(Err(message));
            return;
        }
        match parse_corpus_record_result(&result.stdout) {
            Ok(outcome) => {
                self.append_runtime_log(format!(
                    "[ui] corpus record: {}",
                    corpus_record_log_detail(&outcome)
                ));
                if matches!(outcome, CorpusRecordOutcome::Saved { .. }) {
                    // Refresh the ✓ markers now that a new recording exists.
                    self.ensure_corpus_loaded(true);
                }
                self.corpus_record_result = Some(Ok(outcome));
            }
            Err(error) => {
                let message = format!("Could not read recording result: {error}");
                self.append_runtime_log(format!("[ERROR] {message}"));
                if !result.stderr.trim().is_empty() {
                    self.append_runtime_output(result.stderr.trim_end());
                }
                self.corpus_record_result = Some(Err(message));
            }
        }
    }
}
