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
use crate::runtime::resource_app_root;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Background-task label for the worker's `--record-corpus-item` run. Routed in
/// `poll_background_task` to `apply_corpus_record`, which parses the terminal
/// done/error event into the inline System-tab confirmation.
pub(in crate::ui) const RECORD_CORPUS_ITEM_LABEL: &str = "record corpus item";

/// Breathing gap between batch clips: after one clip's done-event the UI waits
/// this long before launching the next item's worker, so the user can glance at
/// the new reference text and draw breath. Short enough not to feel stalled.
const BATCH_GAP: Duration = Duration::from_millis(1500);

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
        // Cache which items already have a recording so combo_entry_label can
        // check in O(1) with no per-frame filesystem I/O.
        self.corpus_recorded_ids = recorded_ids_set(&appdata, &self.corpus_items);
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
        self.launch_record_for(&id);
    }

    /// Launch the single-item worker for `id`, applying the runtime-stopped gate
    /// and clearing the previous inline result. Shared by the single Record
    /// button and by the batch flow (which chains one launch per item), so both
    /// take the identical proven capture path. The `run_background_command` call
    /// enforces the no-other-task half of the gate; a runtime that is not stopped
    /// is logged and skipped (the batch poll then stops the run on the next tick).
    /// Wave 8 Part 2: the pre-v1.20 corpus-record launch shelled out to
    /// the Python worker via `--record-corpus-item=<id>`. That flag
    /// belonged to the deleted Python bundle; the native-Rust corpus
    /// recorder is tracked as a follow-up. Meanwhile the launch is a
    /// friendly log entry, gated on the same "runtime stopped"
    /// invariant so a user click cannot fight the dictation runtime.
    pub(in crate::ui) fn launch_record_for(&mut self, id: &str) {
        if self.runtime_state != RuntimeState::Stopped {
            let hint = corpus_record_text(
                &self.settings.ui_language,
                CorpusRecordText::StopRuntimeHint,
            );
            self.append_runtime_log(format!("[ui] record corpus item skipped: {hint}"));
            return;
        }
        self.corpus_record_result = None;
        self.append_runtime_log(format!(
            "[ui] record corpus item ({id}): removed in v1.20 with the Python worker; \
             a native-Rust corpus recorder is tracked as a follow-up to #348."
        ));
        // Terminate any active batch immediately -- there is nothing to
        // wait for and no `apply_corpus_record` will run to advance the
        // sequence for us.
        self.corpus_batch = None;
        self.corpus_batch_resume_at = None;
    }

    /// Whether the Record button should be enabled: an item is selected, the
    /// runtime is stopped, and no background task is running. Pure so it is
    /// unit-testable without egui.
    pub(in crate::ui) fn can_record_corpus_item(&self) -> bool {
        self.corpus_selected_id.is_some()
            && self.runtime_state == RuntimeState::Stopped
            && self.background_task.is_none()
    }

    /// Whether a batch run can be STARTED right now: the runtime is stopped, no
    /// background task is running, and we're not already inside a batch. Pure so
    /// the two batch buttons share one truth and it is unit-testable without egui.
    pub(in crate::ui) fn can_start_corpus_batch(&self) -> bool {
        self.runtime_state == RuntimeState::Stopped
            && self.background_task.is_none()
            && self.corpus_batch.is_none()
    }

    /// Whether a batch run is currently active (a sequence is in flight or waiting
    /// out the inter-clip gap). Drives the Stop control and the progress line.
    pub(in crate::ui) fn corpus_batch_active(&self) -> bool {
        self.corpus_batch.is_some()
    }

    /// Start a batch recording run over the items selected by `scope`.
    ///
    /// Computes the target ID list from the loaded corpus + the recorded-ids set
    /// ([`batch_targets`]), and — if non-empty and a run can start — launches the
    /// first item via the SAME single-item worker. Subsequent items are chained in
    /// `apply_corpus_record` (on each done-event) after a small [`BATCH_GAP`]. A
    /// no-op when nothing matches (e.g. "all missing" with everything recorded) or
    /// when a run is already in flight / the runtime is busy.
    pub(in crate::ui) fn start_corpus_batch(&mut self, scope: BatchScope) {
        if !self.can_start_corpus_batch() {
            return;
        }
        let targets = batch_targets(&self.corpus_items, &self.corpus_recorded_ids, scope);
        let Some(batch) = CorpusBatch::new(targets) else {
            // Nothing to do (e.g. every item already recorded) — leave a log
            // breadcrumb so the click isn't a silent no-op.
            self.append_runtime_log("[ui] corpus batch: nothing to record (no matching items)");
            return;
        };
        let total = batch.total();
        let Some(first) = batch.current().map(str::to_owned) else {
            return;
        };
        self.append_runtime_log(format!(
            "[ui] corpus batch start: {total} item(s), scope {scope:?}"
        ));
        self.corpus_batch = Some(batch);
        self.corpus_batch_resume_at = None;
        self.launch_record_for(&first);
    }

    /// Stop an in-flight batch run. Drops the transient cursor and any pending
    /// gap; the current clip's worker (if mid-record) still finishes and saves —
    /// `apply_corpus_record` simply won't advance once the batch is gone.
    pub(in crate::ui) fn stop_corpus_batch(&mut self) {
        if self.corpus_batch.take().is_some() {
            self.corpus_batch_resume_at = None;
            self.append_runtime_log("[ui] corpus batch stopped by user");
        }
    }

    /// Per-frame batch driver: launch the next queued clip once the inter-clip
    /// [`BATCH_GAP`] has elapsed. Called from the app `update` loop. Also defends
    /// the invariant that a batch never outlives a usable runtime — if the runtime
    /// is no longer stopped mid-batch, the run is stopped rather than wedged.
    pub(in crate::ui) fn poll_corpus_batch(&mut self) {
        if self.corpus_batch.is_none() {
            return;
        }
        // If the runtime started (or a foreign task grabbed the slot) we can't
        // keep recording — end the batch cleanly instead of stalling forever.
        if self.runtime_state != RuntimeState::Stopped {
            self.stop_corpus_batch();
            return;
        }
        let Some(resume_at) = self.corpus_batch_resume_at else {
            return;
        };
        if Instant::now() < resume_at || self.background_task.is_some() {
            return;
        }
        self.corpus_batch_resume_at = None;
        let next = self
            .corpus_batch
            .as_ref()
            .and_then(|batch| batch.current())
            .map(str::to_owned);
        match next {
            Some(id) => self.launch_record_for(&id),
            None => self.corpus_batch = None,
        }
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
            // A run that couldn't even start ends the batch — looping a broken
            // launch would just repeat the failure on every item.
            self.abort_batch_on_failure();
            return;
        }
        match parse_corpus_record_result(&result.stdout) {
            Ok(outcome) => {
                self.append_runtime_log(format!(
                    "[ui] corpus record: {}",
                    corpus_record_log_detail(&outcome)
                ));
                let saved = matches!(outcome, CorpusRecordOutcome::Saved { .. });
                if saved {
                    // Refresh the ✓ markers now that a new recording exists.
                    self.ensure_corpus_loaded(true);
                }
                self.corpus_record_result = Some(Ok(outcome));
                if saved {
                    self.batch_after_saved();
                } else {
                    // A worker-reported failure (e.g. no audio captured) stops the
                    // batch so the user can fix the cause instead of recording N
                    // more silent clips.
                    self.abort_batch_on_failure();
                }
            }
            Err(error) => {
                let message = format!("Could not read recording result: {error}");
                self.append_runtime_log(format!("[ERROR] {message}"));
                if !result.stderr.trim().is_empty() {
                    self.append_runtime_output(result.stderr.trim_end());
                }
                self.corpus_record_result = Some(Err(message));
                self.abort_batch_on_failure();
            }
        }
    }

    /// Advance the active batch (if any) after a clip saved: step the cursor and
    /// either schedule the next clip after [`BATCH_GAP`] or finish the run. A no-op
    /// when no batch is active (the single-item Record path). Pure of egui.
    fn batch_after_saved(&mut self) {
        // Advance the cursor and capture what we need (more-remaining? + total)
        // BEFORE touching any other `self` field, so the `batch` borrow is dropped
        // and the follow-up log / field writes don't double-borrow `self`.
        let Some((more, total)) = self
            .corpus_batch
            .as_mut()
            .map(|batch| (batch.advance(), batch.total()))
        else {
            return;
        };
        if more {
            // More to record — arm the inter-clip gap; `poll_corpus_batch`
            // launches the next item once it elapses.
            self.corpus_batch_resume_at = Some(Instant::now() + BATCH_GAP);
        } else {
            self.append_runtime_log(format!(
                "[ui] corpus batch complete: {total} item(s) recorded"
            ));
            self.corpus_batch = None;
            self.corpus_batch_resume_at = None;
        }
    }

    /// End the batch (if any) because the last clip did not save. The already
    /// recorded clips are kept; only the remaining sequence is abandoned.
    fn abort_batch_on_failure(&mut self) {
        if let Some(batch) = self.corpus_batch.take() {
            self.corpus_batch_resume_at = None;
            self.append_runtime_log(format!(
                "[ui] corpus batch stopped after a failed clip ({} of {} recorded)",
                batch.completed(),
                batch.total()
            ));
        }
    }
}
