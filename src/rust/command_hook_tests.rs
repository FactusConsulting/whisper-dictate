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

use super::parse_command;

#[test]
fn command_strings_preserve_windows_and_unc_paths() {
    assert_eq!(
        parse_command(r#""C:\Program Files\Tools\processor.exe" --file "D:\notes\text.txt" "\\server\share\folder\""#).unwrap(),
        [r"C:\Program Files\Tools\processor.exe", "--file", r"D:\notes\text.txt", r"\\server\share\folder\"],
    );
    assert_eq!(
        parse_command(r"tool C:\Tools\ D:\notes\text.txt").unwrap(),
        ["tool", r"C:\Tools\", r"D:\notes\text.txt"]
    );
}

#[test]
fn command_strings_preserve_empty_and_adjacent_quoted_arguments() {
    assert_eq!(
        parse_command(r#"tool "" '' a""b "c d""e""#).unwrap(),
        ["tool", "", "", "ab", "c de"]
    );
}

#[test]
fn command_strings_use_quotes_only_without_shell_escaping() {
    assert_eq!(
        parse_command(r#"tool 'a"b' "it's literal" $VALUE *.txt a\b"#).unwrap(),
        ["tool", "a\"b", "it's literal", "$VALUE", "*.txt", r"a\b"]
    );
    assert!(parse_command("tool 'unfinished").is_err());
    assert!(parse_command("tool \"unfinished").is_err());
}

#[test]
fn json_command_array_preserves_exact_arguments() {
    let args = vec![
        r"C:\Tools\processor.exe",
        "",
        r"\\server\share\",
        "a\"b",
        "'literal'",
    ];
    assert_eq!(
        parse_command(&serde_json::to_string(&args).unwrap()).unwrap(),
        args
    );
}
