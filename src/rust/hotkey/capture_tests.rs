use super::capture::{validate_driver_flag, CaptureEvent, OUTPUT_PREFIX};

#[test]
fn capture_public_contract_keeps_driver_validation_and_output_prefix_stable() {
    validate_driver_flag("auto").expect("auto is a supported capture driver");
    assert_eq!(OUTPUT_PREFIX, "[hotkey-capture]");
    assert!(matches!(
        CaptureEvent::ListenerInstalled {
            driver: "rdev",
            chord: "pause".to_owned(),
        },
        CaptureEvent::ListenerInstalled { .. }
    ));
}
