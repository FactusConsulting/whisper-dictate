use super::hotkey::{capability_from_preflight, hotkey_capability, HotkeyCapability};

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
