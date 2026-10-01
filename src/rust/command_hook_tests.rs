use super::run_argv;
use std::time::{Duration, Instant};

fn fixture_argv(role: &str) -> Vec<String> {
    let command = crate::bounded_process::fixtures::command(role);
    let mut argv = vec![command.get_program().to_string_lossy().into_owned()];
    argv.extend(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned()),
    );
    argv
}

#[test]
fn hook_deadline_also_covers_a_large_event_that_the_child_never_reads() {
    let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
    // run_argv deliberately scrubs only credentials, so the test fixture role
    // is inherited while no production process or global executable is changed.
    let previous = std::env::var_os("WD_HELPER_TEST_ROLE");
    std::env::set_var("WD_HELPER_TEST_ROLE", "blocked-input");
    let started = Instant::now();
    let result = run_argv(
        &fixture_argv("blocked-input"),
        &serde_json::json!({"text": "x".repeat(4 * 1024 * 1024)}),
        Duration::from_millis(300),
    );
    match previous {
        Some(value) => std::env::set_var("WD_HELPER_TEST_ROLE", value),
        None => std::env::remove_var("WD_HELPER_TEST_ROLE"),
    }
    let (code, _, timed_out) = result.unwrap();
    assert!(timed_out);
    assert_eq!(code, None);
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "stdin writes must be included in the 300ms deadline"
    );
}
