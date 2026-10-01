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
