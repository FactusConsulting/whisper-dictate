use super::*;

#[test]
fn parse_duration_accepts_integer_seconds() {
    let d = parse_duration_secs("5").unwrap();
    assert_eq!(d, Duration::from_secs(5));
}

#[test]
fn parse_duration_accepts_fractional_seconds() {
    let d = parse_duration_secs("0.5").unwrap();
    assert_eq!(d, Duration::from_millis(500));
}

#[test]
fn parse_duration_trims_whitespace() {
    let d = parse_duration_secs("  0.25 ").unwrap();
    assert_eq!(d, Duration::from_millis(250));
}

#[test]
fn parse_duration_rejects_non_numeric() {
    let err = parse_duration_secs("foo").unwrap_err().to_string();
    assert!(err.contains("numeric"), "unexpected error: {err}");
}

#[test]
fn parse_duration_rejects_zero_and_negative() {
    assert!(parse_duration_secs("0").is_err());
    assert!(parse_duration_secs("-1").is_err());
    assert!(parse_duration_secs("-0.5").is_err());
}

#[test]
fn parse_duration_rejects_non_finite() {
    assert!(parse_duration_secs("inf").is_err());
    assert!(parse_duration_secs("NaN").is_err());
}

#[test]
fn parse_duration_caps_at_24_hours() {
    // A typo like `--for 999999` shouldn't wedge the tool overnight.
    let d = parse_duration_secs("999999").unwrap();
    assert_eq!(d, Duration::from_secs(24 * 3600));
}

// -----------------------------------------------------------------------
// split_key_names — mirrors runtime::extract_hotkey_key_names behaviour
// -----------------------------------------------------------------------

#[test]
fn split_key_names_single_key() {
    assert_eq!(split_key_names("ctrl_r"), vec!["ctrl_r".to_owned()]);
}

#[test]
fn split_key_names_multi_key_chord() {
    assert_eq!(
        split_key_names("ctrl_l+shift_l+l"),
        vec!["ctrl_l".to_owned(), "shift_l".to_owned(), "l".to_owned(),]
    );
}

#[test]
fn split_key_names_trims_and_drops_empty() {
    assert_eq!(
        split_key_names("  ctrl_l +  + shift_r "),
        vec!["ctrl_l".to_owned(), "shift_r".to_owned()]
    );
}

#[test]
fn split_key_names_empty_input_yields_empty_vec() {
    assert!(split_key_names("").is_empty());
    assert!(split_key_names("   ").is_empty());
    assert!(split_key_names("+ + +").is_empty());
}

#[test]
fn captured_chord_preserves_modifier_side() {
    let mut capture = CapturedChord::default();
    capture.observe(&CaptureEvent::KeyDown {
        t_secs: 0.0,
        name: "ctrl_l".to_owned(),
    });
    capture.observe(&CaptureEvent::KeyDown {
        t_secs: 0.1,
        name: "f9".to_owned(),
    });
    capture.observe(&CaptureEvent::KeyUp {
        t_secs: 0.2,
        name: "f9".to_owned(),
    });
    assert_eq!(
        capture.observe(&CaptureEvent::KeyUp {
            t_secs: 0.3,
            name: "ctrl_l".to_owned(),
        }),
        Some("ctrl_l+f9".to_owned())
    );
}

#[test]
fn captured_chord_rejects_new_keys_after_a_partial_release() {
    let mut capture = CapturedChord::default();
    for (name, pressed, t_secs) in [
        ("ctrl_l", true, 0.0),
        ("f9", true, 0.1),
        ("ctrl_l", false, 0.2),
        ("f10", true, 0.3),
        ("f9", false, 0.4),
        ("f10", false, 0.5),
    ] {
        assert_eq!(
            capture.observe(&if pressed {
                CaptureEvent::KeyDown {
                    t_secs,
                    name: name.to_owned(),
                }
            } else {
                CaptureEvent::KeyUp {
                    t_secs,
                    name: name.to_owned(),
                }
            }),
            None
        );
    }
}

#[test]
fn repeated_keydown_does_not_invalidate_a_partial_candidate() {
    let mut capture = CapturedChord::default();
    for (name, pressed, t_secs) in [
        ("ctrl_l", true, 0.0),
        ("space", true, 0.1),
        ("ctrl_l", false, 0.2),
        ("space", true, 0.3),
        ("space", false, 0.4),
    ] {
        let result = capture.observe(&if pressed {
            CaptureEvent::KeyDown {
                t_secs,
                name: name.to_owned(),
            }
        } else {
            CaptureEvent::KeyUp {
                t_secs,
                name: name.to_owned(),
            }
        });
        if name == "space" && !pressed {
            assert_eq!(result, Some("ctrl_l+space".to_owned()));
        } else {
            assert_eq!(result, None);
        }
    }
}

#[test]
fn captured_chord_accepts_only_installable_names() {
    assert_eq!(capture_key_name("ctrl_r"), Some("ctrl_r".to_owned()));
    assert_eq!(capture_key_name("f12"), Some("f12".to_owned()));
    assert_eq!(capture_key_name("backspace"), None);
    assert_eq!(capture_key_name("f13"), None);
}

#[test]
fn unsupported_member_cancels_the_entire_candidate() {
    let mut capture = CapturedChord::default();
    capture.observe(&CaptureEvent::KeyDown {
        t_secs: 0.0,
        name: "ctrl_l".to_owned(),
    });
    capture.observe(&CaptureEvent::KeyDown {
        t_secs: 0.1,
        name: "a".to_owned(),
    });
    capture.observe(&CaptureEvent::KeyUp {
        t_secs: 0.2,
        name: "a".to_owned(),
    });
    assert_eq!(
        capture.observe(&CaptureEvent::KeyUp {
            t_secs: 0.3,
            name: "ctrl_l".to_owned(),
        }),
        None
    );
    capture.observe(&CaptureEvent::KeyDown {
        t_secs: 0.4,
        name: "ctrl_l".to_owned(),
    });
    capture.observe(&CaptureEvent::KeyDown {
        t_secs: 0.5,
        name: "f9".to_owned(),
    });
    capture.observe(&CaptureEvent::KeyUp {
        t_secs: 0.6,
        name: "f9".to_owned(),
    });
    assert_eq!(
        capture.observe(&CaptureEvent::KeyUp {
            t_secs: 0.7,
            name: "ctrl_l".to_owned(),
        }),
        Some("ctrl_l+f9".to_owned())
    );
}

// -----------------------------------------------------------------------
// resolve_chord_key_names — `--chord` override precedence
// -----------------------------------------------------------------------

mod chord_override {
    use crate::hotkey::capture::events::resolve_chord_key_names;
    use std::io::Write;

    /// Materialise a `settings.json` on disk with the given `key`
    /// value. Returned handle keeps the tempdir alive for the caller's
    /// scope. Kept private because every test in this module needs it.
    fn config_with_key(key: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let contents = format!(r#"{{"key":"{key}","provider":"groq","toggle_mode":false}}"#);
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(contents.as_bytes()).expect("write");
        (dir, path)
    }

    #[test]
    fn override_reaches_coordinator_bypassing_config() {
        // Precondition: config on disk points to a DIFFERENT chord.
        // With the override supplied, the coordinator must see the
        // override, not the config's chord.
        let (_dir, cfg_path) = config_with_key("ctrl_l+shift_l");
        let names = resolve_chord_key_names(Some("ctrl_l+alt_l+f9"), Some(cfg_path.as_path()))
            .expect("override + config resolves");
        assert_eq!(
            names,
            vec!["ctrl_l".to_owned(), "alt_l".to_owned(), "f9".to_owned(),],
            "the override, not the config's chord, must reach the coordinator",
        );
    }

    #[test]
    fn override_without_config_short_circuits_config_read() {
        // The whole point of `--chord`: verifying a chord without
        // touching the user's settings. A missing config path must
        // NOT surface as an error when the override is provided.
        let names = resolve_chord_key_names(Some("shift_r+f9"), None)
            .expect("override alone resolves without touching config");
        assert_eq!(names, vec!["shift_r".to_owned(), "f9".to_owned()]);
    }

    #[test]
    fn override_wins_even_when_config_has_empty_key() {
        // An empty `settings.key` is a hard error in the fallback
        // config path. The override MUST short-circuit that: someone
        // whose config has drifted to empty needs `--chord` to still
        // work so they can test candidate chords before saving one.
        let (_dir, cfg_path) = config_with_key("");
        let names = resolve_chord_key_names(Some("ctrl_l"), Some(cfg_path.as_path()))
            .expect("override wins over empty config key");
        assert_eq!(names, vec!["ctrl_l".to_owned()]);
    }

    #[test]
    fn empty_override_names_the_flag_in_the_error_message() {
        // The two error paths (empty --chord vs empty config key)
        // must surface distinctly so the operator knows WHICH input
        // was empty.
        let err = resolve_chord_key_names(Some("   "), None)
            .expect_err("whitespace override is rejected")
            .to_string();
        assert!(err.contains("--chord"), "error must name the flag: {err}");
        assert!(
            err.contains("+"),
            "error should hint at the `+`-separated format: {err}",
        );
    }

    #[test]
    fn config_error_wording_names_settings_key_when_override_is_none() {
        // Complement of the previous test: when there is no override,
        // the error must talk about `settings.key`, not `--chord`.
        let (_dir, cfg_path) = config_with_key("");
        let err = resolve_chord_key_names(None, Some(cfg_path.as_path()))
            .expect_err("empty config key without override is rejected")
            .to_string();
        assert!(
            err.contains("settings.key"),
            "error must name the config field, not the flag: {err}",
        );
        assert!(
            !err.contains("--chord"),
            "config-side error must not mention the CLI flag: {err}",
        );
    }
}

// -----------------------------------------------------------------------
// format_plain
// -----------------------------------------------------------------------

#[test]
fn only_duration_and_exit_on_chord_are_terminal() {
    let inst = CaptureEvent::ListenerInstalled {
        driver: "rdev",
        chord: "ctrl_r".to_owned(),
    };
    assert!(!inst.is_terminal());
    assert!(!CaptureEvent::KeyDown {
        t_secs: 0.0,
        name: "a".to_owned(),
    }
    .is_terminal());
    assert!(!CaptureEvent::ChordMatched {
        t_secs: 0.0,
        id: 1,
        focused: None,
    }
    .is_terminal());
    assert!(CaptureEvent::DurationReached {
        t_secs: 5.0,
        events: 0,
        chords: 0,
        foreign_keys: 0,
    }
    .is_terminal());
    assert!(CaptureEvent::ExitOnChord {
        t_secs: 0.1,
        events: 0,
        chords: 0,
        foreign_keys: 0,
    }
    .is_terminal());
}

// -----------------------------------------------------------------------
// decide_terminal — the exit-on-chord condition
// -----------------------------------------------------------------------
