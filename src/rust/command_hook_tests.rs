use super::{bounded_timeout_ms, timeout_ms_setting};

#[test]
fn standalone_and_owned_hook_timeouts_use_shared_finite_bounds() {
    for raw in ["NaN", "inf", "-inf", "1e300", "600001", "0", "-1", "bad"] {
        assert_eq!(bounded_timeout_ms(raw.to_owned()), 2000, "{raw}");
    }
    assert_eq!(bounded_timeout_ms("600000".to_owned()), 600000);
    assert_eq!(bounded_timeout_ms("250.9".to_owned()), 250);
    assert_eq!(bounded_timeout_ms("100".to_owned()), 100);
}

#[test]
fn ambient_hook_timeout_cannot_bypass_shared_validation() {
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("VOICEPI_COMMAND_HOOK_TIMEOUT_MS", value),
                None => std::env::remove_var("VOICEPI_COMMAND_HOOK_TIMEOUT_MS"),
            }
        }
    }
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    let _restore = Restore(std::env::var_os("VOICEPI_COMMAND_HOOK_TIMEOUT_MS"));
    for raw in ["NaN", "inf", "1e300", "600001"] {
        std::env::set_var("VOICEPI_COMMAND_HOOK_TIMEOUT_MS", raw);
        assert_eq!(timeout_ms_setting(), 2000, "{raw}");
    }
}
