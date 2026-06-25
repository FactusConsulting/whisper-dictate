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
    /// should see.
    pub fn handle(&mut self, event: &RawKeyEvent) -> Option<TrackerOutput> {
        // Prune stale foreign keys BEFORE every decision so a missed key-up
        // cannot wedge bare-modifier rule 1 / 2 forever. (Target keys are
        // never pruned by timeout — their lifecycle is bracketed by ChordRelease.)
        self.expire_stale_foreign(event.at);
        match event.kind {
            RawKeyKind::Press => self.handle_press(&event.name, event.at),
            RawKeyKind::Release => self.handle_release(&event.name),
        }
    }

    fn handle_press(&mut self, name: &str, at: Instant) -> Option<TrackerOutput> {
        // Key-repeat suppression: if we've already recorded this exact name
        // as pressed, it's an OS repeat. Refresh the timestamp so a key that
        // is *actually* still held keeps blocking past the nominal expiry —
        // mirrors the OS-key-repeat refresh in vp_keys_solo.py.
        if let Some(entry) = self.pressed.get_mut(name) {
            entry.last_seen = at;
            return None;
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
                return Some(TrackerOutput::ChordCancel);
            }
            return None;
        }

        // Target press — check whether this completes the chord.
        if !self.chord_complete() {
            return None;
        }
        if self.chord_latched {
            return None; // already-fired chord, this is a stray repeat path
        }
        // Bare-modifier rule 1: refuse to start if any foreign key is held.
        if self.bare_modifier_binding && self.foreign_key_held() {
            self.chord_latched = true; // latch anyway so subsequent presses don't double-fire
            return None;
        }
        self.chord_latched = true;
        self.chord_emitted = true;
        Some(TrackerOutput::ChordPress)
    }

    fn handle_release(&mut self, name: &str) -> Option<TrackerOutput> {
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
            return None;
        }
        if !self.chord_complete() {
            self.chord_latched = false;
            if self.chord_emitted {
                self.chord_emitted = false;
                return Some(TrackerOutput::ChordRelease);
            }
        }
        None
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
}

fn is_generic_modifier_name(name: &str) -> bool {
    matches!(name, "ctrl" | "shift" | "alt" | "cmd")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }
    fn press_at(name: &str, at: Instant) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind: RawKeyKind::Press,
            at,
        }
    }
    fn release_at(name: &str, at: Instant) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind: RawKeyKind::Release,
            at,
        }
    }
    fn press(name: &str) -> RawKeyEvent {
        press_at(name, t0())
    }
    fn release(name: &str) -> RawKeyEvent {
        release_at(name, t0())
    }

    #[test]
    fn solo_modifier_press_release_emits_chord() {
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("ctrl_l")), Some(TrackerOutput::ChordPress));
        assert_eq!(
            t.handle(&release("ctrl_l")),
            Some(TrackerOutput::ChordRelease)
        );
    }

    #[test]
    fn key_repeat_does_not_re_fire() {
        let mut t = KeyTracker::new(vec!["f9".to_owned()]);
        assert_eq!(t.handle(&press("f9")), Some(TrackerOutput::ChordPress));
        assert_eq!(t.handle(&press("f9")), None);
        assert_eq!(t.handle(&press("f9")), None);
        assert_eq!(t.handle(&release("f9")), Some(TrackerOutput::ChordRelease));
    }

    #[test]
    fn opposite_side_does_not_satisfy_side_specific_target() {
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        // ctrl_r is foreign — for a bare-modifier binding it would only
        // matter if we held ctrl_l first. Standalone right ctrl: no chord.
        assert_eq!(t.handle(&press("ctrl_r")), None);
        assert_eq!(t.handle(&release("ctrl_r")), None);
    }

    #[test]
    fn generic_press_satisfies_side_specific_target_failsafe() {
        // The OS occasionally delivers sideless ctrl — must still satisfy
        // a ctrl_l binding (fail-safe toward starting).
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("ctrl")), Some(TrackerOutput::ChordPress));
        assert_eq!(
            t.handle(&release("ctrl")),
            Some(TrackerOutput::ChordRelease)
        );
    }

    #[test]
    fn chord_completion_needs_two_distinct_held() {
        // ctrl_l+ctrl_r both-sides chord must NOT fire on a single generic
        // ctrl press — that's the 1:1 matching property.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned(), "ctrl_r".to_owned()]);
        assert_eq!(t.handle(&press("ctrl")), None);
        // Adding a second key — a side-specific one — completes the chord
        // via the augmenting-path matching (one generic + one specific).
        assert_eq!(t.handle(&press("ctrl_l")), Some(TrackerOutput::ChordPress));
    }

    #[test]
    fn bare_modifier_with_foreign_key_held_blocks_start_rule1() {
        // Foreign key held FIRST, then PTT chord → rule 1: refuse to start.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("a")), None);
        assert_eq!(t.handle(&press("ctrl_l")), None);
        // Release the foreign key first, then ctrl_l — still no chord
        // since the latch was set to suppress it. (Mirrors vp_keys.py: a
        // late release re-arms only after the chord breaks.)
        assert_eq!(t.handle(&release("a")), None);
        assert_eq!(t.handle(&release("ctrl_l")), None);
    }

    #[test]
    fn bare_modifier_foreign_press_during_recording_cancels_rule2() {
        // Chord held, then foreign key down → cancel.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("ctrl_l")), Some(TrackerOutput::ChordPress));
        assert_eq!(t.handle(&press("c")), Some(TrackerOutput::ChordCancel));
    }

    #[test]
    fn non_bare_binding_ignores_foreign_keys() {
        // f9 alone is NOT a bare-modifier binding — rule 1/2 do not apply.
        let mut t = KeyTracker::new(vec!["f9".to_owned()]);
        assert_eq!(t.handle(&press("a")), None);
        assert_eq!(t.handle(&press("f9")), Some(TrackerOutput::ChordPress));
        // Foreign press during recording: no cancel for non-bare bindings.
        assert_eq!(t.handle(&press("b")), None);
        assert_eq!(t.handle(&release("f9")), Some(TrackerOutput::ChordRelease));
    }

    #[test]
    fn release_of_opposite_side_does_not_break_chord() {
        // Both-sides chord held; release of one side breaks it, release of
        // the other doesn't fire a second time. Mirrors the side-specific
        // release-clearing path.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned(), "ctrl_r".to_owned()]);
        assert_eq!(t.handle(&press("ctrl_l")), None);
        assert_eq!(t.handle(&press("ctrl_r")), Some(TrackerOutput::ChordPress));
        assert_eq!(
            t.handle(&release("ctrl_l")),
            Some(TrackerOutput::ChordRelease)
        );
        // Right side still held — releasing it now is a no-op (the chord
        // already broke on the left release).
        assert_eq!(t.handle(&release("ctrl_r")), None);
    }

    #[test]
    fn generic_release_clears_whole_family() {
        // ctrl_l down, then sideless ctrl up: must clear the held ctrl_l so
        // the chord breaks (fail-safe). Mirrors the generic-release branch.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("ctrl_l")), Some(TrackerOutput::ChordPress));
        assert_eq!(
            t.handle(&release("ctrl")),
            Some(TrackerOutput::ChordRelease)
        );
    }

    // ---------------------------------------------------------------------
    // Foreign-key self-heal (P2: drop entries whose key-up we missed).
    // ---------------------------------------------------------------------

    #[test]
    fn stale_foreign_key_expires_and_unblocks_rule1() {
        // Bare-modifier binding wedge scenario: foreign key "a" is held,
        // and we miss its key-up (Alt+Tab steals focus). After
        // FOREIGN_KEY_EXPIRY the next ctrl_l press must NOT be blocked by
        // rule 1 — the stale entry has been pruned.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        let base = Instant::now();
        assert_eq!(t.handle(&press_at("a", base)), None);
        // Confirm rule 1 actually fires while "a" is still fresh.
        assert_eq!(t.handle(&press_at("ctrl_l", base)), None);
        assert_eq!(t.handle(&release_at("ctrl_l", base)), None);
        // Now jump past the expiry. Pressing ctrl_l should fire ChordPress.
        let later = base + FOREIGN_KEY_EXPIRY + Duration::from_millis(1);
        assert_eq!(
            t.handle(&press_at("ctrl_l", later)),
            Some(TrackerOutput::ChordPress)
        );
    }

    #[test]
    fn foreign_key_repeat_refreshes_expiry() {
        // OS key-repeat for a held foreign key refreshes its timestamp,
        // matching the Python self-heal's behaviour. Without the refresh, a
        // genuinely-held key would falsely "expire" mid-press.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        let base = Instant::now();
        assert_eq!(t.handle(&press_at("a", base)), None);
        // Repeat just inside the window — refreshes last_seen.
        let mid = base + FOREIGN_KEY_EXPIRY - Duration::from_millis(100);
        assert_eq!(t.handle(&press_at("a", mid)), None);
        // Now FOREIGN_KEY_EXPIRY past the ORIGINAL press but still inside
        // the window since the repeat refreshed it. Rule 1 must still fire.
        let probe = base + FOREIGN_KEY_EXPIRY + Duration::from_millis(1);
        assert_eq!(t.handle(&press_at("ctrl_l", probe)), None);
        assert_eq!(t.handle(&release_at("ctrl_l", probe)), None);
    }

    #[test]
    fn target_key_never_expires_even_after_long_idle() {
        // A long pause (model load, transcription) must not silently drop
        // the genuinely-held PTT key — only foreign keys get the timeout.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        let base = Instant::now();
        assert_eq!(
            t.handle(&press_at("ctrl_l", base)),
            Some(TrackerOutput::ChordPress)
        );
        // Long pause well past FOREIGN_KEY_EXPIRY — release must still fire.
        let late = base + FOREIGN_KEY_EXPIRY * 5;
        assert_eq!(
            t.handle(&release_at("ctrl_l", late)),
            Some(TrackerOutput::ChordRelease)
        );
    }
}
