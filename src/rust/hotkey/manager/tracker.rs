//! Pure side-aware press/release tracker — the testable heart of the
//! hotkey manager. Takes [`RawKeyEvent`]s (key name + press/release +
//! timestamp), maintains the held-key set and the rising-edge latch, and
//! emits [`TrackerOutput`]s the coordinator consumes.
//!
//! Always compiled (no `rdev` dependency) so its unit tests run on every CI
//! job. The `rdev` driver in [`super::rdev_driver`] is the thin platform
//! shim that translates real OS events into the same [`RawKeyEvent`] stream.
//!
//! Mirrors the Python `_PynputListener` semantics (vp_keys.py +
//! vp_keys_solo.py) one-for-one so behaviour stays identical when the
//! supervisor swaps backends. In particular:
//!
//! * Bare-modifier rule 1 (refuse start while a foreign key is held).
//! * Bare-modifier rule 2 (cancel if a foreign key joins mid-recording).
//! * Side-specific release-clearing (a generic `ctrl` release drops a held
//!   `ctrl_l`, but not vice-versa).
//! * Foreign-key self-heal: a held foreign key whose release we missed
//!   (Alt+Tab, Win+L, RDP focus loss, ...) expires after
//!   [`FOREIGN_KEY_EXPIRY`] so PTT cannot wedge until restart. Matches the
//!   Python guard in vp_keys_solo.py.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::hotkey::modifier_match::{
    all_targets_have_distinct_match, canonical_side, modifier_family, modifier_matches,
};

/// A held foreign key with no observed key-up self-heals after this many
/// seconds. A real chord forms within ~1 s, so this is comfortably above any
/// genuine chord latency while still recovering from missed key-ups. Mirrors
/// `FOREIGN_KEY_EXPIRY_S` in vp_keys_solo.py.
pub const FOREIGN_KEY_EXPIRY: Duration = Duration::from_secs(10);

/// A single OS key event after name normalisation. Pure data; produced by
/// the `rdev` driver in production and by hand-written fixtures in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawKeyEvent {
    pub name: String,
    pub kind: RawKeyKind,
    /// Monotonic timestamp of when the event was observed. Used to expire
    /// stale foreign keys (rule 1 self-heal). The rdev driver fills this
    /// with [`Instant::now`]; tests can inject a synthetic clock.
    pub at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKeyKind {
    Press,
    Release,
}

/// What the tracker tells the coordinator about a stream of raw events. One
/// raw event can produce zero or one outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerOutput {
    /// PTT chord just became complete (rising edge — never key-repeat).
    ChordPress,
    /// PTT chord just broke (falling edge).
    ChordRelease,
    /// A foreign key joined the held PTT modifier(s) — discard the in-flight
    /// recording. Mirrors the bare-modifier rule-2 path in vp_keys.py.
    ChordCancel,
}

/// Detailed classification of what happened when the tracker processed a
/// raw event — richer than [`TrackerOutput`] because it also names the
/// paths that produce no output (target key blocked by rule 1, target
/// press while chord was already latched, non-target press ignored, ...).
///
/// Added for the Windows PTT wedge investigation (see
/// [`crate::hotkey::diag`]) so the rdev driver can distinguish "the
/// tracker rejected this press for a specific reason" from "not our key,
/// nothing to do" in the diagnostic log. Callers that only care about the
/// side-effect can keep using [`KeyTracker::handle`] — this enum is
/// exposed via [`KeyTracker::handle_verbose`] as a strict superset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerDecision {
    /// Emitted [`TrackerOutput::ChordPress`] — the chord just started.
    ChordPress,
    /// Emitted [`TrackerOutput::ChordRelease`] — the chord just ended.
    ChordRelease,
    /// Emitted [`TrackerOutput::ChordCancel`] — a foreign key joined an
    /// active recording (bare-modifier rule 2).
    ChordCancel,
    /// A target-key press that we already had in the pressed map — OS
    /// key-repeat. Silent; no output.
    TargetKeyRepeat,
    /// A target-key press that did NOT complete the chord because a
    /// multi-key binding is still waiting for its other key(s). Silent;
    /// no output. `held` lists the currently-pressed key names in
    /// unspecified order.
    TargetChordIncomplete { held: Vec<String> },
    /// A target-key press blocked by bare-modifier rule 1 (a foreign key
    /// is still held). **This is the primary suspect for the Windows
    /// wedge in hypothesis (B)**: stale entries left in `pressed` by
    /// injected events keep triggering rule 1 for every subsequent PTT.
    /// `foreign_held` lists the offending key names.
    TargetBlockedByForeignKey { foreign_held: Vec<String> },
    /// A target-key press that arrived while `chord_latched` was true —
    /// the chord was already fired and this event is a stray repeat
    /// path. Silent; no output.
    TargetAlreadyLatched,
    /// A non-target press. Silent; no output. Not counted as a
    /// "rejection" for [`crate::hotkey::diag::Counters::tracker_target_rejects`].
    NonTargetPress,
    /// A non-target release. Silent; no output.
    NonTargetRelease,
    /// A target-key release that produced no output (chord was not
    /// active, or was blocked at start time and never emitted a
    /// `ChordPress` to pair with).
    TargetReleaseNoOp,
}

impl TrackerDecision {
    /// The [`TrackerOutput`] this decision produced, if any. Bridges
    /// [`KeyTracker::handle_verbose`] back to the classic
    /// [`KeyTracker::handle`] return type.
    pub fn output(&self) -> Option<TrackerOutput> {
        match self {
            Self::ChordPress => Some(TrackerOutput::ChordPress),
            Self::ChordRelease => Some(TrackerOutput::ChordRelease),
            Self::ChordCancel => Some(TrackerOutput::ChordCancel),
            _ => None,
        }
    }

    /// True iff this decision represents a **target-key press** that the
    /// tracker declined to turn into a `ChordPress`. Used by the rdev
    /// driver to bump the diagnostic
    /// [`crate::hotkey::diag::Counters::tracker_target_rejects`] counter
    /// only on the "real" rejection paths — not on
    /// [`Self::NonTargetPress`] which is the noisy majority of a normal
    /// typing session and would otherwise drown the signal.
    pub fn is_target_reject(&self) -> bool {
        matches!(
            self,
            Self::TargetKeyRepeat
                | Self::TargetChordIncomplete { .. }
                | Self::TargetBlockedByForeignKey { .. }
                | Self::TargetAlreadyLatched
        )
    }

    /// Short human-readable tag for diagnostic logs. Kept stable — the
    /// heartbeat / debug lines are grepped by users pasting them into
    /// bug reports.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ChordPress => "chord_press",
            Self::ChordRelease => "chord_release",
            Self::ChordCancel => "chord_cancel",
            Self::TargetKeyRepeat => "target_key_repeat",
            Self::TargetChordIncomplete { .. } => "target_chord_incomplete",
            Self::TargetBlockedByForeignKey { .. } => "target_blocked_by_foreign_key",
            Self::TargetAlreadyLatched => "target_already_latched",
            Self::NonTargetPress => "non_target_press",
            Self::NonTargetRelease => "non_target_release",
            Self::TargetReleaseNoOp => "target_release_no_op",
        }
    }
}

/// Per-held-key bookkeeping: the canonical-side form recorded at press time
/// (so opposite-side releases don't collapse a still-held opposite side) and
/// the last activity timestamp (refreshed by OS key-repeat) for the
/// foreign-key self-heal.
#[derive(Debug, Clone)]
struct HeldEntry {
    canonical: String,
    last_seen: Instant,
}

/// Side-aware press/release tracker. Holds the SET of currently-pressed keys
/// (by normalised name) and the rising-edge latch. Clone-free,
/// allocation-light, no I/O — designed to be unit-tested with synthetic
/// [`RawKeyEvent`] streams.
pub struct KeyTracker {
    targets: Vec<String>,
    /// Pressed key name -> held bookkeeping. Foreign-key entries carry a
    /// timestamp so a missed key-up self-heals after [`FOREIGN_KEY_EXPIRY`].
    pressed: HashMap<String, HeldEntry>,
    chord_latched: bool,
    /// True iff the last ChordPress we emitted is still "in flight" — set on
    /// the press that actually emits ChordPress, cleared on the release that
    /// emits ChordRelease (or on a cancel). Distinct from `chord_latched`,
    /// which suppresses repeats whether or not we actually fired (rule 1
    /// blocks a start but still latches so the next repeat does not double-
    /// fire). Without this we'd emit a spurious ChordRelease for a press
    /// that was suppressed by rule 1.
    chord_emitted: bool,
    /// True when the binding is made up entirely of bare modifiers — the
    /// bare-modifier "press alone" rules apply (rule 1 + rule 2 in
    /// vp_keys_solo). When false, foreign keys are ignored.
    bare_modifier_binding: bool,
}

impl KeyTracker {
    /// Build a tracker for `targets` (the user's PTT setting, already split
    /// on `+`). Names use the same convention as the Python settings:
    /// `ctrl_l`, `shift_r`, `alt_gr`, `f9`, ...
    pub fn new(targets: Vec<String>) -> Self {
        let bare_modifier_binding =
            !targets.is_empty() && targets.iter().all(|n| modifier_family(n).is_some());
        Self {
            targets,
            pressed: HashMap::new(),
            chord_latched: false,
            chord_emitted: false,
            bare_modifier_binding,
        }
    }

    /// Process one OS event and return what (if anything) the coordinator
    /// should see. Thin wrapper over [`Self::handle_verbose`] for callers
    /// that only care about the coordinator-visible side-effect.
    pub fn handle(&mut self, event: &RawKeyEvent) -> Option<TrackerOutput> {
        self.handle_verbose(event).output()
    }

    /// Process one OS event and return a [`TrackerDecision`] describing
    /// exactly what happened — including the "no output" paths (rule 1
    /// block, key-repeat, chord not yet complete, non-target press, ...).
    /// The rdev driver uses this to feed the diagnostic heartbeat and
    /// the per-event `[hotkey-diag]` debug lines with a specific reason
    /// instead of a bare "tracker returned None".
    ///
    /// Semantically equivalent to [`Self::handle`] on the state
    /// transition: this function does not change the mutation shape of
    /// either the `pressed` map or the latches — it only reports what
    /// was already happening.
    pub fn handle_verbose(&mut self, event: &RawKeyEvent) -> TrackerDecision {
        // Prune stale foreign keys BEFORE every decision so a missed key-up
        // cannot wedge bare-modifier rule 1 / 2 forever. (Target keys are
        // never pruned by timeout — their lifecycle is bracketed by ChordRelease.)
        self.expire_stale_foreign(event.at);
        match event.kind {
            RawKeyKind::Press => self.handle_press(&event.name, event.at),
            RawKeyKind::Release => self.handle_release(&event.name),
        }
    }

    fn handle_press(&mut self, name: &str, at: Instant) -> TrackerDecision {
        // Key-repeat suppression: if we've already recorded this exact name
        // as pressed, it's an OS repeat. Refresh the timestamp so a key that
        // is *actually* still held keeps blocking past the nominal expiry —
        // mirrors the OS-key-repeat refresh in vp_keys_solo.py.
        if let Some(entry) = self.pressed.get_mut(name) {
            entry.last_seen = at;
            // A repeat of a target key is a target-side reject (the caller
            // is holding PTT and getting no re-emit); a repeat of a
            // foreign key is just noise from an unrelated held key.
            return if self.is_target(name) {
                TrackerDecision::TargetKeyRepeat
            } else {
                TrackerDecision::NonTargetPress
            };
        }
        self.pressed.insert(
            name.to_owned(),
            HeldEntry {
                canonical: canonical_side(name).to_owned(),
                last_seen: at,
            },
        );

        let is_target = self.is_target(name);
        if !is_target {
            // Foreign key. Only meaningful if the binding is bare-modifier:
            // a fresh foreign press while we ACTUALLY emitted a ChordPress
            // for the held chord cancels. (If rule 1 had blocked the press
            // there is no recording to cancel — chord_emitted stays false.)
            if self.bare_modifier_binding && self.chord_emitted {
                self.chord_emitted = false;
                return TrackerDecision::ChordCancel;
            }
            return TrackerDecision::NonTargetPress;
        }

        // Target press — check whether this completes the chord.
        if !self.chord_complete() {
            return TrackerDecision::TargetChordIncomplete {
                held: self.pressed.keys().cloned().collect(),
            };
        }
        if self.chord_latched {
            return TrackerDecision::TargetAlreadyLatched; // already-fired chord, stray repeat path
        }
        // Bare-modifier rule 1: refuse to start if any foreign key is held.
        if self.bare_modifier_binding && self.foreign_key_held() {
            self.chord_latched = true; // latch anyway so subsequent presses don't double-fire
            return TrackerDecision::TargetBlockedByForeignKey {
                foreign_held: self.foreign_held_names(),
            };
        }
        self.chord_latched = true;
        self.chord_emitted = true;
        TrackerDecision::ChordPress
    }

    fn handle_release(&mut self, name: &str) -> TrackerDecision {
        // Side-aware release clearing — mirrors held_keys_cleared_by_release
        // in vp_keys_solo.py so press/release pairs (ctrl_l down, generic
        // ctrl up) reconcile correctly.
        let family = modifier_family(name);
        let drop_names: Vec<String> = match family {
            None => self
                .pressed
                .keys()
                .filter(|k| k.as_str() == name)
                .cloned()
                .collect(),
            Some(_) => {
                let r_canonical = canonical_side(name);
                let generic_release = is_generic_modifier_name(name);
                self.pressed
                    .iter()
                    .filter(|(k, entry)| {
                        modifier_family(k) == family
                            && (generic_release
                                || entry.canonical.as_str() == r_canonical
                                || is_generic_modifier_name(k))
                    })
                    .map(|(k, _)| k.clone())
                    .collect()
            }
        };
        for k in &drop_names {
            self.pressed.remove(k);
        }
        if !self.is_target(name) {
            return TrackerDecision::NonTargetRelease;
        }
        if !self.chord_complete() {
            self.chord_latched = false;
            if self.chord_emitted {
                self.chord_emitted = false;
                return TrackerDecision::ChordRelease;
            }
        }
        TrackerDecision::TargetReleaseNoOp
    }

    /// Drop foreign-key entries whose last observed activity is older than
    /// [`FOREIGN_KEY_EXPIRY`]. Target keys are never expired by timeout —
    /// their lifecycle is bracketed by the explicit ChordRelease path so a
    /// genuinely held PTT key over a long Processing pause doesn't get
    /// silently dropped.
    fn expire_stale_foreign(&mut self, now: Instant) {
        let targets = &self.targets;
        let cutoff = FOREIGN_KEY_EXPIRY;
        self.pressed.retain(|name, entry| {
            // Cheap target check inlined so we don't borrow `self` twice.
            let is_target = targets.iter().any(|t| modifier_matches(name, t));
            if is_target {
                return true;
            }
            now.saturating_duration_since(entry.last_seen) < cutoff
        });
    }

    fn is_target(&self, name: &str) -> bool {
        self.targets.iter().any(|t| modifier_matches(name, t))
    }

    fn chord_complete(&self) -> bool {
        let held: HashSet<String> = self.pressed.keys().cloned().collect();
        all_targets_have_distinct_match(&self.targets, &held)
    }

    fn foreign_key_held(&self) -> bool {
        self.pressed.keys().any(|k| !self.is_target(k))
    }

    /// Names of every currently-held foreign key. Used only by the
    /// diagnostic [`TrackerDecision::TargetBlockedByForeignKey`] payload
    /// — the fast [`Self::foreign_key_held`] short-circuit above is what
    /// the hot path uses.
    fn foreign_held_names(&self) -> Vec<String> {
        self.pressed
            .keys()
            .filter(|k| !self.is_target(k))
            .cloned()
            .collect()
    }

    /// Snapshot of every currently-held key name — used by the rdev
    /// driver's diagnostic log to show `held_before` / `held_after`
    /// around every event, so a wedge caused by stale entries in
    /// `pressed` (hypothesis B) is visible directly in the trace.
    /// Sorted so log lines are diff-able across runs.
    pub fn held_snapshot(&self) -> Vec<String> {
        let mut names: Vec<String> = self.pressed.keys().cloned().collect();
        names.sort_unstable();
        names
    }

    /// `chord_latched` — exposed for the diagnostic log so we can see
    /// whether the tracker thinks the chord is currently "in flight".
    /// A wedge scenario where `chord_latched=true` never clears would
    /// silently drop every re-press as `TargetAlreadyLatched`.
    pub fn is_chord_latched(&self) -> bool {
        self.chord_latched
    }

    /// `chord_emitted` — exposed for the diagnostic log. Tells us
    /// whether the last ChordPress had a matching ChordRelease still
    /// pending. Diverging from `chord_latched` in a wedge trace would
    /// suggest a release path miscount.
    pub fn is_chord_emitted(&self) -> bool {
        self.chord_emitted
    }
}

fn is_generic_modifier_name(name: &str) -> bool {
    matches!(name, "ctrl" | "shift" | "alt" | "cmd")
}

/// Unit tests live in a sibling file (`tracker_tests.rs`) so this
/// module stays under the repo-wide 500-LOC per-file rule after the
/// diagnostic [`TrackerDecision`] additions. Included via `#[path]` so
/// the tests continue to see the private items through `use super::*`.
#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tests;
