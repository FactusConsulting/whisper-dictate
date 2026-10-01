//! keys regression tests for the rdev driver.

use super::*;

#[test]
fn rdev_name_set_covers_every_emitted_key() {
    // Every name the rdev->name mapping can emit must appear in the
    // supported-names set so the install-time validator never rejects
    // a name we DO support. If you add a key in `key_to_name`, add it
    // to `RDEV_SUPPORTED_NAMES` (and adjust this assertion if you also
    // expose a new bare-modifier alias).
    for key in [
        rdev::Key::ControlLeft,
        rdev::Key::ControlRight,
        rdev::Key::ShiftLeft,
        rdev::Key::ShiftRight,
        rdev::Key::Alt,
        rdev::Key::AltGr,
        rdev::Key::MetaLeft,
        rdev::Key::MetaRight,
        rdev::Key::F1,
        rdev::Key::F12,
        rdev::Key::Space,
        rdev::Key::Escape,
        rdev::Key::Tab,
        rdev::Key::Return,
        rdev::Key::Pause,
    ] {
        let ev = rdev::Event {
            event_type: rdev::EventType::KeyPress(key),
            time: std::time::SystemTime::UNIX_EPOCH,
            name: None,
        };
        let raw = raw_from_rdev(&ev).expect("mapped or synthetic name");
        // Reject synthetic names for known keys — they must be in the
        // real map, otherwise `is_rdev_supported_name` will not accept them.
        assert!(
            !raw.name.starts_with("__rdev_"),
            "key {key:?} produced synthetic name {name}; add it to key_to_name",
            name = raw.name
        );
        assert!(
            is_rdev_supported_name(&raw.name),
            "rdev emits {name} but install-time validator rejects it",
            name = raw.name
        );
    }
}

#[test]
fn unsupported_names_are_rejected_by_validator() {
    // Names accepted by the other input backends but NOT by rdev. Without
    // the validator a configuration containing one would never fire.
    for name in ["super_l", "super_r", "menu", "scroll_lock"] {
        assert!(
            !is_rdev_supported_name(name),
            "rdev driver claims to support {name} — update the test or the map",
        );
    }
}

// -----------------------------------------------------------------------
// Right-Alt aliases map to the supported modifier name.
// -----------------------------------------------------------------------

#[test]
fn right_alt_and_ralt_aliases_are_accepted_by_validator() {
    // Users and documentation sometimes refer to AltGr as "right_alt"
    // or "ralt". The install-time validator must accept these so the
    // Rust backend doesn't reject a valid AltGr PTT binding.
    for name in ["right_alt", "ralt"] {
        assert!(
            is_rdev_supported_name(name),
            "{name} should be accepted as an AltGr alias",
        );
    }
}

// -----------------------------------------------------------------------
// #656 r3663653258 — rdev fallback must accept every
// side-specific alias that `parse_chord` rejects on the RegisterHotKey
// path. Without this, `win_l+f9`, `win_r+f9`, and `alt_r+f9` bindings
// installed the RegisterHotKey backend, were rejected as side-specific,
// and then hit `UnsupportedKey` on the promised rdev fallback.
// -----------------------------------------------------------------------

#[test]
fn side_specific_aliases_rejected_by_register_are_accepted_by_rdev() {
    for name in ["alt_r", "win_l", "win_r", "win"] {
        assert!(
            is_rdev_supported_name(name),
            "{name} must be accepted by the rdev install-time validator so \
             the RegisterHotKey→rdev fallback works (#656 r3663653258)",
        );
    }
}

// -----------------------------------------------------------------------
// Unmapped ordinary keys still reach the tracker.
// -----------------------------------------------------------------------

#[test]
fn raw_from_rdev_produces_event_for_unmapped_key() {
    // Keys not in key_to_name (e.g. letter keys) must still produce a
    // RawKeyEvent so the tracker can detect foreign-key holds and emit
    // ChordCancel for bare-modifier bindings (rule 2). Previously
    // raw_from_rdev returned None for these, silently dropping them.
    use rdev::{Event, EventType};

    let press_a = Event {
        event_type: EventType::KeyPress(rdev::Key::KeyA),
        time: std::time::SystemTime::UNIX_EPOCH,
        name: None,
    };
    let raw = raw_from_rdev(&press_a);
    assert!(
        raw.is_some(),
        "ordinary key press must produce a RawKeyEvent for foreign-key tracking"
    );
    let raw = raw.unwrap();
    assert!(
        raw.name.starts_with("__rdev_"),
        "unmapped key should use synthetic __rdev_ name, got {:?}",
        raw.name
    );
    assert_eq!(raw.kind, RawKeyKind::Press);
}
