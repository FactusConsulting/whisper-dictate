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
