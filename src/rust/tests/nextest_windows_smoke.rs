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
fn native_windows_smoke_runs_only_in_the_anchor_ci_feature_cell() {
    let workflow = include_str!("../../../.github/workflows/test.yml").replace("\r\n", "\n");
    assert!(workflow.contains(
        "ANCHOR_PROFILE: ${{ github.event_name == 'pull_request' && 'base' || 'shipping' }}"
    ));
    let test_steps = workflow
        .split("      - name: Rust tests\n")
        .nth(1)
        .expect("main Rust tests step")
        .split("      - name: Windows release-script fixture\n")
        .next()
        .unwrap();
    let (anchor_step, non_anchor_step) = test_steps
        .split_once("      - name: Rust tests (Windows non-anchor)\n")
        .expect("separate Windows non-anchor test step");
    assert!(
        anchor_step.contains("runner.os != 'Windows' || matrix.profile.id == env.ANCHOR_PROFILE")
    );
    assert!(anchor_step.contains("-E 'not(binary(=repository_policy))'"));
    assert!(non_anchor_step.contains("runner.os == 'Windows'"));
    assert!(non_anchor_step.contains("matrix.profile.id != env.ANCHOR_PROFILE"));
    assert!(non_anchor_step.contains("not(test(=ui::app_tests::view::hidden_windows_event_loop_drains_runtime_and_tray_without_ui))"));
}
