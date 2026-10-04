use super::hotkey::{capability_from_preflight, hotkey_capability, HotkeyCapability};

#[cfg(all(windows, feature = "rust-hotkeys"))]
#[test]
fn copy_last_validation_rejects_unsupported_chords_and_ptt_collisions() {
    use super::hotkey::validate_copy_last_hotkey;
    assert!(validate_copy_last_hotkey("", "pause", "").is_ok());
    assert!(validate_copy_last_hotkey("ctrl+shift+f8", "pause", "").is_ok());
    for invalid in ["ctrl+shift+f13", "ctrl_r+f8", "ctrl+shift", "ctrl+f12"] {
        assert!(
            validate_copy_last_hotkey(invalid, "pause", "").is_err(),
            "{invalid}"
        );
    }
    assert!(validate_copy_last_hotkey("ctrl+f8", "ctrl+f8", "")
        .unwrap_err()
        .contains("must differ"));
    assert!(validate_copy_last_hotkey("ctrl+f8", "ctrl_r+f9", "")
        .unwrap_err()
        .contains("needs a PTT chord"));
    // With paste-last enabled, a ctrl+v copy binding would re-trigger
    // copy-last on every paste burst (Codex P2 win_registerhotkey.rs:572).
    assert!(
        validate_copy_last_hotkey("ctrl+v", "pause", "ctrl+shift+f9")
            .unwrap_err()
            .contains("re-trigger copy-last")
    );
    // A blank paste-last binding keeps ctrl+v available for copy-last.
    assert!(validate_copy_last_hotkey("ctrl+v", "pause", "").is_ok());
}

#[cfg(all(windows, feature = "rust-hotkeys"))]
#[test]
fn paste_last_validation_rejects_ptt_and_copy_last_collisions() {
    use super::hotkey::validate_paste_last_hotkey;
    assert!(validate_paste_last_hotkey("", "pause", "").is_ok());
    assert!(validate_paste_last_hotkey("ctrl+shift+f9", "pause", "ctrl+shift+f8").is_ok());
    assert!(validate_paste_last_hotkey("ctrl+f9", "ctrl+f9", "")
        .unwrap_err()
        .contains("must differ"));
    assert!(validate_paste_last_hotkey("ctrl+f8", "pause", "ctrl+f8")
        .unwrap_err()
        .contains("copy-last and paste-last"));
    // A blank copy-last binding skips the cross-action collision check.
    assert!(validate_paste_last_hotkey("ctrl+f8", "pause", "").is_ok());
    // The paste arm injects ctrl+v itself, so that exact binding would
    // re-trigger the shortcut the moment the burst starts.
    assert!(validate_paste_last_hotkey("ctrl+v", "pause", "")
        .unwrap_err()
        .contains("re-trigger"));
    // A superset modifier still differs from the injected plain ctrl+v.
    assert!(validate_paste_last_hotkey("ctrl+shift+v", "pause", "").is_ok());
    // Symmetric conflict (Codex P2 win_registerhotkey.rs:572): when
    // copy-last already owns ctrl+v, enabling paste-last would make
    // every paste burst re-trigger copy-last.
    assert!(
        validate_paste_last_hotkey("ctrl+shift+f9", "pause", "ctrl+v")
            .unwrap_err()
            .contains("re-trigger copy-last")
    );
    // Symmetric conflict (Codex P2 win_registerhotkey.rs:620): when
    // PTT already owns ctrl+v, enabling paste-last would start an
    // unintended recording on every paste burst.
    assert!(validate_paste_last_hotkey("ctrl+shift+f9", "ctrl+v", "")
        .unwrap_err()
        .contains("re-trigger PTT"));
}

#[test]
fn invalid_chord_is_distinct_from_native_capability() {
    assert!(matches!(
        hotkey_capability("not-a-key"),
        HotkeyCapability::Invalid(_)
    ));
}

#[test]
fn non_native_named_keys_are_reported() {
    assert!(matches!(
        hotkey_capability("insert"),
        HotkeyCapability::Unsupported(_)
    ));
}

#[cfg(feature = "rust-hotkeys")]
#[test]
fn pause_preflight_names_a_concrete_driver() {
    match hotkey_capability("pause") {
        HotkeyCapability::Installable { planned_driver }
        | HotkeyCapability::FallbackRisk { planned_driver, .. } => {
            assert_ne!(planned_driver, "auto");
            assert!(!planned_driver.is_empty());
        }
        other => panic!("pause should be supported by the shipping listener: {other:?}"),
    }
}

#[test]
fn capability_reducer_distinguishes_installable_and_focus_risk() {
    let installable = capability_from_preflight(Ok(crate::hotkey::HotkeyPreflight {
        planned_driver: "win_registerhotkey",
        fallback_reason: None,
        focused_window_risk: false,
    }));
    assert_eq!(
        installable,
        HotkeyCapability::Installable {
            planned_driver: "win_registerhotkey".to_owned()
        }
    );

    let risk = capability_from_preflight(Ok(crate::hotkey::HotkeyPreflight {
        planned_driver: "rdev",
        fallback_reason: Some("side-specific modifier".to_owned()),
        focused_window_risk: true,
    }));
    assert!(matches!(
        risk,
        HotkeyCapability::FallbackRisk {
            planned_driver,
            reason: Some(_)
        } if planned_driver == "rdev"
    ));
}
