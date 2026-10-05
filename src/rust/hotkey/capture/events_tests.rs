use super::*;

fn parse_json(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSON {line:?}: {e}"))
}

#[test]
fn plain_install_line_has_driver_and_chord() {
    let line = format_plain(&CaptureEvent::ListenerInstalled {
        driver: "rdev",
        chord: "ctrl_l+shift_l+l".to_owned(),
    });
    assert!(line.starts_with(OUTPUT_PREFIX), "prefix: {line}");
    assert!(line.contains("driver=rdev"));
    assert!(line.contains("chord=ctrl_l+shift_l+l"));
}

// Issue #258 acceptance detail 2: the CLI capture path canonicalizes
// modifier aliases through the same `canonical_side` logic the runtime
// matcher uses, so a chord captured as one alias form binds (and matches
// at runtime) consistently with presses delivered in another.
#[test]
fn capture_key_name_canonicalises_modifier_aliases() {
    // AltGr aliases all bind as the right-alt side name.
    for alias in ["alt_gr", "right_alt", "ralt"] {
        assert_eq!(
            super::capture_key_name(alias).as_deref(),
            Some("alt_r"),
            "alias {alias}"
        );
    }
    // Windows-terminology aliases normalise to the cmd_* side names rdev
    // actually emits.
    assert_eq!(super::capture_key_name("win_l").as_deref(), Some("cmd_l"));
    assert_eq!(super::capture_key_name("win_r").as_deref(), Some("cmd_r"));
}

#[test]
fn capture_key_name_preserves_modifier_side_and_ordinary_keys() {
    // Deliberate side preservation: what the user presses is what binds.
    assert_eq!(super::capture_key_name("CTRL_L").as_deref(), Some("ctrl_l"));
    assert_eq!(super::capture_key_name("ctrl_r").as_deref(), Some("ctrl_r"));
    assert_eq!(
        super::capture_key_name("shift_l").as_deref(),
        Some("shift_l")
    );
    // Function and named keys pass through; ordinary typing does not.
    assert_eq!(super::capture_key_name("f9").as_deref(), Some("f9"));
    assert_eq!(super::capture_key_name("pause").as_deref(), Some("pause"));
    assert_eq!(super::capture_key_name("a"), None);
    assert_eq!(super::capture_key_name("1"), None);
    assert_eq!(super::capture_key_name("__rdev_Unknown"), None);
}

#[test]
fn captured_chord_releases_the_whole_held_set_with_aliases_canonicalised() {
    // Issue #258 acceptance detail 1: the chord recorded at release is the
    // full set of keys held, with alias names canonicalised (ralt binds as
    // alt_r).
    let mut captured = CapturedChord::default();
    assert!(captured
        .observe(&CaptureEvent::KeyDown {
            t_secs: 0.0,
            name: "ralt".to_owned()
        })
        .is_none());
    assert!(captured
        .observe(&CaptureEvent::KeyDown {
            t_secs: 0.1,
            name: "f9".to_owned()
        })
        .is_none());
    assert!(captured
        .observe(&CaptureEvent::KeyUp {
            t_secs: 0.2,
            name: "f9".to_owned()
        })
        .is_none());
    let chord = captured
        .observe(&CaptureEvent::KeyUp {
            t_secs: 0.3,
            name: "ralt".to_owned(),
        })
        .expect("the last keyup completes the chord");
    assert_eq!(chord, "alt_r+f9");
    // The state is clean after a release, so a fresh press and release
    // yields another complete chord.
    assert!(captured
        .observe(&CaptureEvent::KeyDown {
            t_secs: 0.4,
            name: "f12".to_owned()
        })
        .is_none());
    let second = captured
        .observe(&CaptureEvent::KeyUp {
            t_secs: 0.5,
            name: "f12".to_owned(),
        })
        .expect("a fresh press and release completes another chord");
    assert_eq!(second, "f12");
}

#[path = "parsing_tests.rs"]
mod parsing;

#[path = "output_tests.rs"]
mod output;
