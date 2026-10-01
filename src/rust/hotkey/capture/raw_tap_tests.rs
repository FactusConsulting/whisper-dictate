#[cfg(feature = "rust-hotkeys")]
mod counters {
    use super::super::*;
    use crate::hotkey::manager::{RawKeyEvent, RawKeyKind, RawTap};
    use std::sync::mpsc;
    use std::time::Instant;

    fn ev(name: &str, kind: RawKeyKind) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind,
            at: Instant::now(),
        }
    }

    fn tap_with(targets: &[&str]) -> (Arc<Counters>, impl RawTap) {
        let counters = Arc::new(Counters::default());
        let (tx, rx) = mpsc::channel();
        // Keep the receiver alive for the tap's lifetime; the events
        // themselves are covered by the formatter tests.
        std::mem::forget(rx);
        let tap = build_raw_tap(
            Arc::clone(&counters),
            tx,
            Instant::now(),
            targets.iter().map(|s| (*s).to_owned()).collect(),
            true,
        );
        (counters, tap)
    }

    #[test]
    fn chord_keys_are_not_counted_as_foreign() {
        let (counters, tap) = tap_with(&["ctrl_l"]);
        tap.tap(&ev("ctrl_l", RawKeyKind::Press));
        tap.tap(&ev("ctrl_l", RawKeyKind::Release));
        assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 0);
        assert_eq!(counters.events.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn foreign_press_is_counted() {
        let (counters, tap) = tap_with(&["ctrl_l"]);
        tap.tap(&ev("shift_l", RawKeyKind::Press));
        assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn auto_repeat_counts_one_physical_press() {
        // A held key emits a stream of key-downs with no interleaved
        // key-up. The maintainer's real capture logged ~20 for one held
        // Shift; counting events would report 20 foreign keys.
        let (counters, tap) = tap_with(&["ctrl_l"]);
        for _ in 0..20 {
            tap.tap(&ev("shift_l", RawKeyKind::Press));
        }
        assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 1);
        // Released and pressed again -- that IS a second press.
        tap.tap(&ev("shift_l", RawKeyKind::Release));
        tap.tap(&ev("shift_l", RawKeyKind::Press));
        assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn distinct_foreign_keys_count_separately() {
        let (counters, tap) = tap_with(&["ctrl_l"]);
        tap.tap(&ev("shift_l", RawKeyKind::Press));
        tap.tap(&ev("alt_l", RawKeyKind::Press));
        assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn generic_modifier_target_matches_concrete_side() {
        // A `ctrl` binding must treat `ctrl_l` as part of the chord, the
        // same way the tracker's `is_target` does. Re-deriving this with
        // string equality would count it as foreign.
        let (counters, tap) = tap_with(&["ctrl"]);
        tap.tap(&ev("ctrl_l", RawKeyKind::Press));
        tap.tap(&ev("ctrl_r", RawKeyKind::Press));
        assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn chord_only_tap_counts_but_never_emits_unrelated_key_identity() {
        let counters = Arc::new(Counters::default());
        let (tx, rx) = mpsc::channel();
        let tap = build_raw_tap(
            Arc::clone(&counters),
            tx,
            Instant::now(),
            vec!["pause".to_owned()],
            false,
        );

        tap.tap(&ev("a", RawKeyKind::Press));

        assert_eq!(counters.events.load(Ordering::Relaxed), 1);
        assert_eq!(counters.foreign_keys.load(Ordering::Relaxed), 1);
        assert!(rx.try_recv().is_err());
    }
}

// -----------------------------------------------------------------------
// Coordinator loop-closing (the "deaf after the first chord" regression)
// -----------------------------------------------------------------------
