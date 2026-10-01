use super::*;

#[test]
fn plain_key_events_include_timestamp_and_name() {
    let down = format_plain(&CaptureEvent::KeyDown {
        t_secs: 0.123,
        name: "ctrl_l".to_owned(),
    });
    assert!(down.contains("0.123s"), "line: {down}");
    assert!(down.contains("ctrl_l DOWN"));
    let up = format_plain(&CaptureEvent::KeyUp {
        t_secs: 1.5,
        name: "shift_r".to_owned(),
    });
    assert!(up.contains("1.500s"));
    assert!(up.contains("shift_r UP"));
}

#[test]
fn plain_chord_events_report_matched_released_canceled() {
    let matched = format_plain(&CaptureEvent::ChordMatched {
        t_secs: 0.1,
        id: 7,
        focused: Some(false),
    });
    let released = format_plain(&CaptureEvent::ChordReleased {
        t_secs: 0.5,
        id: 7,
        focused: Some(false),
    });
    let canceled = format_plain(&CaptureEvent::ChordCanceled {
        t_secs: 0.6,
        id: 8,
        focused: Some(true),
    });
    assert!(matched.contains("CHORD MATCHED"));
    assert!(released.contains("CHORD RELEASED"));
    assert!(canceled.contains("CHORD CANCELED"));
    // The id is exposed so operators can pair matched with released.
    assert!(matched.contains("id=7"));
    assert!(released.contains("id=7"));
    assert!(canceled.contains("id=8"));
}

#[test]
fn plain_duration_reached_includes_summary_counters() {
    let line = format_plain(&CaptureEvent::DurationReached {
        t_secs: 5.0,
        events: 12,
        chords: 3,
        foreign_keys: 1,
    });
    assert!(line.contains("duration reached"));
    assert!(line.contains("Events: 12"));
    assert!(line.contains("Chords: 3"));
    assert!(line.contains("Foreign keys: 1"));
}

#[test]
fn plain_exit_on_chord_includes_summary_counters() {
    let line = format_plain(&CaptureEvent::ExitOnChord {
        t_secs: 0.2,
        events: 3,
        chords: 1,
        foreign_keys: 0,
    });
    assert!(line.contains("exit-on-chord"));
    assert!(line.contains("Events: 3"));
    assert!(line.contains("Chords: 1"));
}

// -----------------------------------------------------------------------
// format_json
// -----------------------------------------------------------------------

#[test]
fn json_install_line_has_kind_driver_chord() {
    let v = parse_json(&format_json(&CaptureEvent::ListenerInstalled {
        driver: "rdev",
        chord: "ctrl_r".to_owned(),
    }));
    assert_eq!(v["kind"], "listener_installed");
    assert_eq!(v["driver"], "rdev");
    assert_eq!(v["chord"], "ctrl_r");
}

#[test]
fn json_key_events_have_kind_t_name() {
    let down = parse_json(&format_json(&CaptureEvent::KeyDown {
        t_secs: 0.123,
        name: "ctrl_l".to_owned(),
    }));
    assert_eq!(down["kind"], "key_down");
    assert_eq!(down["name"], "ctrl_l");
    assert_eq!(down["t"], 0.123);

    let up = parse_json(&format_json(&CaptureEvent::KeyUp {
        t_secs: 0.145,
        name: "shift_l".to_owned(),
    }));
    assert_eq!(up["kind"], "key_up");
    assert_eq!(up["name"], "shift_l");
}

#[test]
fn json_chord_events_have_id_and_kind() {
    let matched = parse_json(&format_json(&CaptureEvent::ChordMatched {
        t_secs: 0.167,
        id: 42,
        focused: Some(false),
    }));
    assert_eq!(matched["kind"], "chord_matched");
    assert_eq!(matched["id"], 42);
    assert_eq!(matched["focused"], false);
    let released = parse_json(&format_json(&CaptureEvent::ChordReleased {
        t_secs: 0.412,
        id: 42,
        focused: Some(true),
    }));
    assert_eq!(released["kind"], "chord_released");
    assert_eq!(released["focused"], true);
    let canceled = parse_json(&format_json(&CaptureEvent::ChordCanceled {
        t_secs: 0.5,
        id: 43,
        focused: None,
    }));
    assert_eq!(canceled["kind"], "chord_canceled");
    assert!(canceled["focused"].is_null());
}

#[test]
fn json_terminal_events_carry_counters() {
    let dur = parse_json(&format_json(&CaptureEvent::DurationReached {
        t_secs: 5.0,
        events: 7,
        chords: 1,
        foreign_keys: 0,
    }));
    assert_eq!(dur["kind"], "duration_reached");
    assert_eq!(dur["events"], 7);
    assert_eq!(dur["chords"], 1);
    assert_eq!(dur["foreign_keys"], 0);

    let onchord = parse_json(&format_json(&CaptureEvent::ExitOnChord {
        t_secs: 0.3,
        events: 3,
        chords: 1,
        foreign_keys: 0,
    }));
    assert_eq!(onchord["kind"], "exit_on_chord");
    assert_eq!(onchord["chords"], 1);
}

#[test]
fn json_t_field_is_rounded_to_three_decimals() {
    // Guards against `0.12300000000000001` sneaking into the machine-
    // readable output — tests that pin the JSON contract would break
    // otherwise.
    let line = format_json(&CaptureEvent::KeyDown {
        t_secs: 0.1230000000001,
        name: "a".to_owned(),
    });
    assert!(line.contains("\"t\":0.123"), "unexpected: {line}");
}

// -----------------------------------------------------------------------
// is_terminal
// -----------------------------------------------------------------------
