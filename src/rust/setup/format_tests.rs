use super::{bash_lines, config_json, export_text, powershell_lines};
use std::collections::BTreeMap;

fn values() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("key".to_owned(), "it's ctrl".to_owned()),
        ("lang".to_owned(), "dansk æøå".to_owned()),
    ])
}

#[test]
fn shell_quoting_handles_quotes_and_unicode() {
    let ps = powershell_lines(&values(), &BTreeMap::new());
    assert!(ps.contains("$env:VOICEPI_KEY = 'it''s ctrl'"));
    assert!(ps.contains("$env:VOICEPI_LANG = 'dansk æøå'"));
    let bash = bash_lines(&values(), &BTreeMap::new());
    assert!(bash.contains("export VOICEPI_KEY='it'\\''s ctrl'"));
    assert!(bash.contains("export VOICEPI_LANG='dansk æøå'"));
}

#[test]
fn config_json_uses_schema_keys() {
    let json = config_json(&values()).unwrap();
    assert!(json.contains("\"key\": \"it's ctrl\""));
    assert!(json.contains("\"lang\": \"dansk æøå\""));
}

#[test]
fn explicit_nullable_clear_exports_as_json_null_and_empty_shell_assignment() {
    let config = BTreeMap::from([("lang".to_owned(), String::new())]);

    let json = config_json(&config).unwrap();
    assert!(json.contains("\"lang\": null"));
    assert!(powershell_lines(&config, &BTreeMap::new()).contains("$env:VOICEPI_LANG = ''"));
    assert!(bash_lines(&config, &BTreeMap::new()).contains("export VOICEPI_LANG=''"));
}

#[test]
fn secrets_are_redacted_unless_explicitly_included() {
    let secrets = BTreeMap::from([("VOICEPI_STT_API_KEY".to_owned(), "secret-value".to_owned())]);
    let hidden = export_text(&values(), &secrets, false).unwrap();
    assert!(hidden.contains("VOICEPI_STT_API_KEY = '***'"));
    assert!(!hidden.contains("secret-value"));
    let shown = export_text(&values(), &secrets, true).unwrap();
    assert!(shown.contains("secret-value"));
}
