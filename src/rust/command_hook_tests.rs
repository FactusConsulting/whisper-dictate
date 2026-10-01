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
