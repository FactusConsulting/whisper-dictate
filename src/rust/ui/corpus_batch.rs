//! Pure batch-recording model for the System tab's "record corpus audio" cluster.
//!
//! The single-item flow records one picked item via the `--record-corpus-item`
//! worker. Batch recording walks a *list* of items in turn, reusing that SAME
//! proven single-item worker for each clip and chaining them in the UI: when one
//! clip's done-event arrives, the UI advances to the next target and launches it
//! again. There is deliberately NO Python batch mode — each clip still takes the
//! single-item path that already negotiates the capture device correctly.
//!
//! This module holds only the *pure* part of that flow so it unit-tests without
//! egui or a worker:
//!
//!   * [`batch_targets`] computes the ordered list of IDs to record for a given
//!     [`BatchScope`] (`AllMissing` = items without a recording yet; `All` =
//!     every item, re-recording included), from the corpus + the recorded-ids set.
//!   * [`CorpusBatch`] is the transient (never persisted) progress cursor: which
//!     target is current, how many are done, and the advance/stop transitions the
//!     done-event handler drives.
//!
//! The egui rendering and the worker-launch wiring live in `tabs/corpus_record.rs`
//! and `corpus_record_tasks.rs`; this file is intentionally free of both.

use super::corpus::CorpusItem;
use std::collections::HashSet;

/// Which corpus items a batch run targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum BatchScope {
    /// Only items that do NOT yet have a recording — the common "finish the
    /// corpus" case. Skips everything already captured.
    AllMissing,
    /// Every item, re-recording the ones that already have audio (each re-record
    /// overwrites only that item's own WAV; the dictionary is never touched).
    All,
}

/// The ordered list of corpus item IDs a batch run will record for `scope`.
///
/// Preserves the corpus's own order (so the user reads top-to-bottom as listed)
/// and, for [`BatchScope::AllMissing`], drops items whose ID is already in
/// `recorded_ids`. Pure: no I/O, no egui — the recorded-ids set is supplied by
/// the caller (the UI caches it in `corpus_recorded_ids`).
pub(in crate::ui) fn batch_targets(
    items: &[CorpusItem],
    recorded_ids: &HashSet<String>,
    scope: BatchScope,
) -> Vec<String> {
    items
        .iter()
        .filter(|item| match scope {
            BatchScope::All => true,
            BatchScope::AllMissing => !recorded_ids.contains(&item.id),
        })
        .map(|item| item.id.clone())
        .collect()
}

/// Transient cursor over a batch recording run. NEVER persisted — it lives only
/// for the duration of one "Record all (missing)" sequence and is dropped on
/// completion or when the user stops.
///
/// The flow is UI-driven: [`current`](Self::current) is the ID to record now;
/// after that clip's worker reports done, [`advance`](Self::advance) steps to the
/// next target (returning `true` while more remain), and the UI launches it
/// again. Stopping is modelled by the owner setting its `Option<CorpusBatch>` to
/// `None` — there is no explicit stop method, because dropping the cursor is all
/// the cleanup needed. `index` counts *completed* targets, so it doubles as the
/// "k of N" progress numerator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct CorpusBatch {
    /// The ordered IDs still framing this run (computed once by [`batch_targets`]).
    targets: Vec<String>,
    /// How many targets have completed — also the 0-based index of the current
    /// target. Equals `targets.len()` exactly when the run has finished.
    index: usize,
}

impl CorpusBatch {
    /// Start a batch over `targets` (already filtered/ordered by [`batch_targets`]
    /// for the chosen [`BatchScope`]). Returns `None` when there is nothing to
    /// record so the caller never starts an empty run (e.g. "Record all missing"
    /// when everything is already done).
    pub(in crate::ui) fn new(targets: Vec<String>) -> Option<Self> {
        if targets.is_empty() {
            return None;
        }
        Some(Self { targets, index: 0 })
    }

    /// The ID to record right now, or `None` once every target is done.
    pub(in crate::ui) fn current(&self) -> Option<&str> {
        self.targets.get(self.index).map(String::as_str)
    }

    /// Total number of targets in this run (the "N" in "k of N").
    pub(in crate::ui) fn total(&self) -> usize {
        self.targets.len()
    }

    /// 1-based position of the current target (the "k" in "k of N"); clamped to
    /// `total()` once the run has finished so the label never reads past the end.
    pub(in crate::ui) fn position(&self) -> usize {
        (self.index + 1).min(self.targets.len())
    }

    /// Number of targets already completed.
    pub(in crate::ui) fn completed(&self) -> usize {
        self.index
    }

    /// Whether every target has been recorded (the run is finished).
    pub(in crate::ui) fn is_finished(&self) -> bool {
        self.index >= self.targets.len()
    }

    /// Mark the current target done and step to the next. Returns `true` while a
    /// further target remains (the caller should launch [`current`]), `false`
    /// when the run is now finished (the caller drops the batch). Idempotent past
    /// the end: advancing a finished run stays finished and returns `false`.
    pub(in crate::ui) fn advance(&mut self) -> bool {
        if self.index < self.targets.len() {
            self.index += 1;
        }
        !self.is_finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> CorpusItem {
        CorpusItem {
            id: id.to_owned(),
            text: format!("text for {id}"),
            language: "da".to_owned(),
        }
    }

    fn items() -> Vec<CorpusItem> {
        vec![item("a"), item("b"), item("c")]
    }

    fn recorded(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    // ── batch_targets ─────────────────────────────────────────────────────────

    #[test]
    fn all_scope_targets_every_item_in_order() {
        let targets = batch_targets(&items(), &recorded(&[]), BatchScope::All);
        assert_eq!(targets, vec!["a", "b", "c"]);
    }

    #[test]
    fn all_scope_includes_already_recorded_items() {
        // "All" re-records everything, even items that already have audio.
        let targets = batch_targets(&items(), &recorded(&["a", "c"]), BatchScope::All);
        assert_eq!(targets, vec!["a", "b", "c"]);
    }

    #[test]
    fn all_missing_scope_drops_recorded_items_keeps_order() {
        let targets = batch_targets(&items(), &recorded(&["b"]), BatchScope::AllMissing);
        assert_eq!(targets, vec!["a", "c"]);
    }

    #[test]
    fn all_missing_scope_is_empty_when_everything_recorded() {
        let targets = batch_targets(
            &items(),
            &recorded(&["a", "b", "c"]),
            BatchScope::AllMissing,
        );
        assert!(targets.is_empty());
    }

    #[test]
    fn targets_are_empty_for_an_empty_corpus() {
        assert!(batch_targets(&[], &recorded(&[]), BatchScope::All).is_empty());
        assert!(batch_targets(&[], &recorded(&[]), BatchScope::AllMissing).is_empty());
    }

    // ── CorpusBatch::new ──────────────────────────────────────────────────────

    #[test]
    fn new_is_none_for_an_empty_target_list() {
        assert!(CorpusBatch::new(Vec::new()).is_none());
    }

    #[test]
    fn new_starts_at_the_first_target() {
        let batch = CorpusBatch::new(vec!["a".into(), "b".into()]).unwrap();
        assert_eq!(batch.current(), Some("a"));
        assert_eq!(batch.total(), 2);
        assert_eq!(batch.position(), 1);
        assert_eq!(batch.completed(), 0);
        assert!(!batch.is_finished());
    }

    // ── advance / stop ────────────────────────────────────────────────────────

    #[test]
    fn advance_steps_through_targets_then_finishes() {
        let mut batch = CorpusBatch::new(vec!["a".into(), "b".into(), "c".into()]).unwrap();

        // a → b
        assert!(batch.advance(), "more remain after the first");
        assert_eq!(batch.current(), Some("b"));
        assert_eq!(batch.position(), 2);
        assert_eq!(batch.completed(), 1);

        // b → c
        assert!(batch.advance(), "more remain after the second");
        assert_eq!(batch.current(), Some("c"));
        assert_eq!(batch.position(), 3);
        assert_eq!(batch.completed(), 2);

        // c → done
        assert!(!batch.advance(), "no more remain after the last");
        assert!(batch.is_finished());
        assert_eq!(batch.current(), None);
        assert_eq!(batch.completed(), 3);
        // Position is clamped to total once finished (never reads "4 of 3").
        assert_eq!(batch.position(), 3);
    }

    #[test]
    fn advance_past_the_end_is_idempotent() {
        let mut batch = CorpusBatch::new(vec!["only".into()]).unwrap();
        assert!(
            !batch.advance(),
            "single-target run finishes on first advance"
        );
        assert!(batch.is_finished());
        // Advancing again must not move the cursor or panic.
        assert!(!batch.advance());
        assert!(batch.is_finished());
        assert_eq!(batch.completed(), 1);
        assert_eq!(batch.current(), None);
    }

    #[test]
    fn dropping_the_batch_models_a_stop() {
        // "Stop" is modelled by the owner setting its Option<CorpusBatch> to None;
        // there is no in-progress residue to clean up because the batch is the
        // only transient state. A partially advanced batch simply ceases to exist.
        let mut batch = CorpusBatch::new(vec!["a".into(), "b".into(), "c".into()]).unwrap();
        batch.advance(); // recorded "a", now on "b"
        assert_eq!(batch.current(), Some("b"));
        // Owner stops: drop. Nothing else references the cursor.
        drop(batch);
    }
}
