//! Unit test for the VOICEPI_HEADLESS_SMOKE early-exit path in `ui::run()`.

#[test]
fn headless_smoke_env_var_exits_ok() {
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // SAFETY: ENV_LOCK is held; no other test in this binary may
    // concurrently read or write env vars until the guard drops.
    unsafe { std::env::set_var("VOICEPI_HEADLESS_SMOKE", "1") };
    let result = super::run();
    unsafe { std::env::remove_var("VOICEPI_HEADLESS_SMOKE") };
    assert!(
        result.is_ok(),
        "ui::run() must return Ok(()) when VOICEPI_HEADLESS_SMOKE is set; got: {result:?}"
    );
}
