use super::*;

#[test]
fn typed_snapshot_keeps_credentials_out_of_debug_output() {
    let snapshot = RuntimeSettingsSnapshot::from_pairs([
        ("VOICEPI_LANG".to_owned(), "da".to_owned()),
        (STT_API_KEY_ENV.to_owned(), "sentinel-secret".to_owned()),
    ])
    .unwrap();

    assert_eq!(snapshot.settings().lang, "da");
    assert_eq!(snapshot.value(STT_API_KEY_ENV), Some("sentinel-secret"));
    let debug = format!("{snapshot:?}");
    assert!(debug.contains(STT_API_KEY_ENV));
    assert!(!debug.contains("sentinel-secret"));
}

#[test]
fn explicit_values_outrank_ambient_without_mutating_the_process() {
    let snapshot = RuntimeSettingsSnapshot::from_pairs_with_ambient(
        [
            ("VOICEPI_LANG".to_owned(), "en".to_owned()),
            (POST_API_KEY_ENV.to_owned(), "saved-post".to_owned()),
        ],
        |name| match name {
            "VOICEPI_LANG" => Some("da".to_owned()),
            POST_API_KEY_ENV => Some("ambient-post".to_owned()),
            _ => None,
        },
    )
    .unwrap();

    assert_eq!(snapshot.settings().lang, "en");
    assert_eq!(snapshot.value(POST_API_KEY_ENV), Some("saved-post"));
    assert!(!snapshot.credential_is_ambient(POST_API_KEY_ENV, "saved-post"));
}

#[test]
fn child_process_cannot_inherit_scoped_or_ambient_credentials() {
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args([
            "/d",
            "/c",
            "if defined VOICEPI_STT_API_KEY (exit /b 7) else (exit /b 0)",
        ]);
        command
    } else {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "test -z \"$VOICEPI_STT_API_KEY\""]);
        command
    };
    command.env(STT_API_KEY_ENV, "sentinel-child-secret");
    scrub_credentials_from_child(&mut command);

    assert!(command.status().unwrap().success());
}
