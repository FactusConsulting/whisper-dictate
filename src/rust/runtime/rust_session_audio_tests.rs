//! Guards for the production push-to-talk capture wiring (#323). The
//! lifecycle itself is covered with a fake opener in
//! `capture_lifecycle_tests.rs` and `capture_sink_tests.rs`.

use super::{CaptureOpener, RawCaptureOpener};

#[test]
fn only_the_push_to_talk_opener_starts_cpal_capture() {
    let audio = include_str!("rust_session_audio.rs");
    assert_eq!(
        audio.matches("RawCapturePipeline::start(").count(),
        1,
        "capture may only start inside RawCaptureOpener::open"
    );
    for (name, source) in [
        (
            "rust_session_real_backends.rs",
            include_str!("rust_session_real_backends.rs"),
        ),
        ("rust_session_sink.rs", include_str!("rust_session_sink.rs")),
        ("capture_lifecycle.rs", include_str!("capture_lifecycle.rs")),
    ] {
        assert!(
            !source.contains("RawCapturePipeline::start("),
            "{name} must not open the microphone outside a recording"
        );
    }
}

#[test]
fn session_build_does_not_keep_an_always_on_pump() {
    let backends = include_str!("rust_session_real_backends.rs");
    assert!(backends.contains("SessionAudio::for_session"));
    assert!(
        !backends.contains("spawn_for_session"),
        "the always-open audio pump must not come back"
    );
}

#[test]
fn plain_open_errors_are_not_classified_as_driver_timeouts() {
    assert!(!RawCaptureOpener.is_timeout(&anyhow::anyhow!("input device not found")));
}

#[test]
fn probing_an_unknown_input_fails_without_opening_a_stream() {
    let error = RawCaptureOpener
        .probe("whisper-dictate-test-device-that-does-not-exist")
        .unwrap_err();
    assert!(!error.to_string().is_empty());
}
