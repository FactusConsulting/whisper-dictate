use std::sync::atomic::AtomicBool;

use super::*;

#[test]
fn hex_lower_encodes_each_nibble_in_lowercase() {
    assert_eq!(hex_lower(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
}

#[test]
fn stopped_runtime_rejects_download_work() {
    let stopped = AtomicBool::new(false);
    let error = ensure_runtime_active(&stopped, "model").expect_err("stopped runtime must cancel");

    assert!(error.to_string().contains("download cancelled"));
}
