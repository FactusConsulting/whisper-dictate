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

#[test]
fn native_windows_smoke_runs_in_the_shipping_suite() {
    let workflow = include_str!("../../../.github/workflows/test.yml").replace("\r\n", "\n");
    let shipping_tests = workflow
        .split("      - name: Rust tests\n")
        .nth(1)
        .and_then(|step| step.split("\n      - name: ").next())
        .expect("Windows shipping tests step");
    assert!(shipping_tests.contains("runner.os == 'Windows'"));
    assert!(shipping_tests.contains("matrix.profile.id == 'shipping'"));
    assert!(shipping_tests.contains("cargo nextest run"));
    assert!(shipping_tests.contains("-E 'not(binary(=repository_policy))'"));
    assert!(
        !shipping_tests.contains("hidden_windows_event_loop_drains_runtime_and_tray_without_ui")
    );
}
