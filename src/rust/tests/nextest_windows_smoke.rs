//! Preserve CI resource isolation without weakening the GUI smoke assertions.
#[test]
fn native_windows_smoke_reserves_the_entire_nextest_pool() {
    let config = include_str!("../.config/nextest.toml");
    let override_block = config.split("[[profile.ci.overrides]]").find(|block| {
        block.contains("test(=ui::app_tests::view::hidden_windows_event_loop_drains_runtime_and_tray_without_ui)")
    }).expect("Windows native smoke override");
    assert!(override_block.contains("platform = 'cfg(windows)'"));
    assert!(override_block.contains("threads-required = 'num-test-threads'"));
    assert!(
        !override_block.contains("retries"),
        "retries must not mask a wedged event loop"
    );
}
