//! Coverage for the session limits boundary.

use super::tests_support::*;

#[test]
fn invalid_direct_session_recording_caps_fall_back_to_two_minutes() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _snapshot = EnvVarSnapshot::new(&["VOICEPI_MAX_RECORD_S"]);
    for invalid in ["-1", "NaN", "inf", "not-a-number"] {
        std::env::set_var("VOICEPI_MAX_RECORD_S", invalid);
        assert_eq!(
            super::max_record_samples_from_env(),
            Some(120 * 16_000),
            "{invalid:?} must retain the safety ceiling"
        );
    }
    std::env::set_var("VOICEPI_MAX_RECORD_S", "0");
    assert_eq!(
        super::max_record_samples_from_env(),
        None,
        "only explicit zero disables the cap"
    );
}
