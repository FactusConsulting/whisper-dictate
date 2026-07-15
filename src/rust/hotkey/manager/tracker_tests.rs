//! Unit tests for the parent tracker module — extracted into a sibling
//! file so `tracker.rs` stays under the repo-wide 500-LOC per file rule
//! (see `AGENTS.md`) after the diagnostic `TrackerDecision` additions.
//! Included via `#[path]` so the module still sees the tracker's private
//! items via `use super::*` at the same visibility level.

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

// ---------------------------------------------------------------------
// TrackerDecision — richer classification for the diagnostic path
// (`crate::hotkey::diag`). handle_verbose must return the same
// side-effect as handle(), but with a specific reason for every
// "no output" branch so we can distinguish A/B in a wedge log.
// ---------------------------------------------------------------------

#[test]
fn handle_verbose_reports_chord_press_and_release_for_solo_binding() {
    let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
    assert_eq!(
        t.handle_verbose(&press("ctrl_l")),
        TrackerDecision::ChordPress
    );
    assert_eq!(
        t.handle_verbose(&release("ctrl_l")),
        TrackerDecision::ChordRelease
    );
}

#[test]
fn handle_verbose_names_target_key_repeat_reason() {
    // OS key-repeat of a held target must surface as TargetKeyRepeat
    // rather than a generic "None" — the diagnostic bumps
    // tracker_target_rejects for this and it's important to know a
    // wedge log flooded with repeats isn't tracker misbehaviour.
    let mut t = KeyTracker::new(vec!["f9".to_owned()]);
    assert_eq!(t.handle_verbose(&press("f9")), TrackerDecision::ChordPress);
    assert_eq!(
        t.handle_verbose(&press("f9")),
        TrackerDecision::TargetKeyRepeat
    );
}

#[test]
fn handle_verbose_names_rule1_block_with_foreign_key_list() {
    // The Windows wedge signature: a foreign key sneaks in (via
    // injection feedback or a missed key-up) and blocks every
    // subsequent PTT chord until FOREIGN_KEY_EXPIRY expires.
    // TargetBlockedByForeignKey must name the offender so the diag
    // log points at it.
    let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
    assert_eq!(
        t.handle_verbose(&press("a")),
        TrackerDecision::NonTargetPress
    );
    let decision = t.handle_verbose(&press("ctrl_l"));
    match &decision {
        TrackerDecision::TargetBlockedByForeignKey { foreign_held } => {
            assert!(
                foreign_held.iter().any(|k| k == "a"),
                "foreign_held should mention 'a', got {foreign_held:?}"
            );
        }
        other => panic!("expected TargetBlockedByForeignKey, got {other:?}"),
    }
    assert!(decision.is_target_reject());
    assert_eq!(decision.output(), None);
}

#[test]
fn handle_verbose_names_chord_incomplete_for_multi_key_binding() {
    // A multi-key chord binding whose second key is not yet held.
    let mut t = KeyTracker::new(vec!["ctrl_l".to_owned(), "shift_l".to_owned()]);
    let decision = t.handle_verbose(&press("ctrl_l"));
    match &decision {
        TrackerDecision::TargetChordIncomplete { held } => {
            assert!(held.iter().any(|k| k == "ctrl_l"));
        }
        other => panic!("expected TargetChordIncomplete, got {other:?}"),
    }
    assert!(decision.is_target_reject());
}

#[test]
fn handle_verbose_names_non_target_press_and_release() {
    // Foreign traffic (ordinary typing) must classify as
    // NonTargetPress / NonTargetRelease and MUST NOT trip
    // is_target_reject — otherwise every character the user types
    // during a happy session would inflate tracker_target_rejects
    // and obscure the real signal.
    let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
    let d = t.handle_verbose(&press("z"));
    assert_eq!(d, TrackerDecision::NonTargetPress);
    assert!(!d.is_target_reject());
    let d = t.handle_verbose(&release("z"));
    assert_eq!(d, TrackerDecision::NonTargetRelease);
    assert!(!d.is_target_reject());
}

#[test]
fn tracker_decision_output_matches_tracker_handle_return() {
    // Contract test: for every event stream, handle().output ==
    // handle_verbose().output(). Guards against a future edit that
    // splits the two.
    let mut a = KeyTracker::new(vec!["ctrl_l".to_owned()]);
    let mut b = KeyTracker::new(vec!["ctrl_l".to_owned()]);
    let events = [
        press("z"),
        press("ctrl_l"),
        release("ctrl_l"),
        release("z"),
        press("ctrl_l"),
        release("ctrl_l"),
    ];
    for e in &events {
        let via_handle = a.handle(e);
        let via_verbose = b.handle_verbose(e).output();
        assert_eq!(
            via_handle, via_verbose,
            "output divergence for event {e:?}: handle={via_handle:?} verbose={via_verbose:?}"
        );
    }
}
