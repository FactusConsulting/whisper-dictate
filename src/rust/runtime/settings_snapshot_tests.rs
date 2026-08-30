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
fn provider_ownership_is_explicit_instead_of_inferred_from_typed_defaults() {
    let mut snapshot = RuntimeSettingsSnapshot::from_pairs([]).unwrap();
    assert_eq!(snapshot.stt_provider(), "openai");
    assert!(!snapshot.has_explicit_stt_provider());

    snapshot.set_stt_provider("nemotron");

    assert_eq!(snapshot.stt_provider(), "nemotron");
    assert!(snapshot.has_explicit_stt_provider());
}

#[test]
fn selecting_nemotron_migrates_legacy_endpoint_in_owned_snapshot() {
    let mut snapshot = RuntimeSettingsSnapshot::from_pairs([(
        "VOICEPI_STT_BASE_URL".to_owned(),
        "http://localhost:9000/v1".to_owned(),
    )])
    .unwrap();

    snapshot.set_stt_provider("nemotron");

    assert_eq!(snapshot.settings().stt_base_url, "grpc://localhost:50051");
    assert_eq!(
        snapshot.value("VOICEPI_STT_BASE_URL"),
        Some("grpc://localhost:50051")
    );
    assert_eq!(snapshot.initial_stt_base_url(), "grpc://localhost:50051");
}

#[test]
fn selecting_in_process_nemotron_preserves_cuda_device_override() {
    let mut snapshot = RuntimeSettingsSnapshot::from_pairs([
        ("VOICEPI_STT_BACKEND".to_owned(), "openai".to_owned()),
        (
            "VOICEPI_STT_BASE_URL".to_owned(),
            "inproc://nemotron".to_owned(),
        ),
        (
            "VOICEPI_STT_MODEL".to_owned(),
            "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        ),
        ("VOICEPI_DEVICE".to_owned(), "cuda".to_owned()),
    ])
    .unwrap();

    snapshot.set_stt_provider("nemotron");
    assert_eq!(snapshot.settings().device, "cuda");

    snapshot.set("VOICEPI_DEVICE", "cuda").unwrap();
    assert_eq!(snapshot.settings().device, "cuda");
}

#[test]
fn providerless_in_process_nemotron_uses_inferred_provider_for_device_override() {
    let mut snapshot = RuntimeSettingsSnapshot::from_pairs([
        ("VOICEPI_STT_BACKEND".to_owned(), "openai".to_owned()),
        (
            "VOICEPI_STT_BASE_URL".to_owned(),
            "inproc://nemotron".to_owned(),
        ),
        (
            "VOICEPI_STT_MODEL".to_owned(),
            "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        ),
    ])
    .unwrap();
    assert!(!snapshot.has_explicit_stt_provider());
    assert_eq!(snapshot.settings().stt_provider, "nemotron");

    snapshot.set("VOICEPI_DEVICE", "cuda").unwrap();

    assert_eq!(snapshot.settings().stt_provider, "nemotron");
    assert_eq!(snapshot.settings().device, "cuda");
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

#[test]
fn compatibility_values_are_captured_once_with_explicit_precedence() {
    let snapshot = RuntimeSettingsSnapshot::from_pairs_with_ambient(
        [(
            "VOICEPI_WHISPER_MODEL_PATH".to_owned(),
            "explicit.bin".to_owned(),
        )],
        |name| match name {
            "VOICEPI_WHISPER_MODEL_PATH" => Some("ambient.bin".to_owned()),
            crate::whisper::IDLE_UNLOAD_ENV => Some("45".to_owned()),
            crate::whisper::GPU_ENV => Some("vulkan".to_owned()),
            _ => None,
        },
    )
    .unwrap();

    assert_eq!(
        snapshot.value("VOICEPI_WHISPER_MODEL_PATH"),
        Some("explicit.bin")
    );
    assert_eq!(snapshot.value(crate::whisper::IDLE_UNLOAD_ENV), Some("45"));
    assert_eq!(snapshot.value(crate::whisper::GPU_ENV), Some("vulkan"));
}

#[test]
fn set_rebuilds_typed_settings_and_clears_ambient_credential_provenance() {
    let mut snapshot = RuntimeSettingsSnapshot::from_pairs_with_ambient([], |name| {
        (name == STT_API_KEY_ENV).then(|| "ambient-secret".to_owned())
    })
    .unwrap();
    assert!(snapshot.credential_is_ambient(STT_API_KEY_ENV, "ambient-secret"));

    snapshot.set("VOICEPI_LANG", "en").unwrap();
    snapshot.set(STT_API_KEY_ENV, "owned-secret").unwrap();
    assert_eq!(snapshot.settings().lang, "en");
    assert_eq!(snapshot.value(STT_API_KEY_ENV), Some("owned-secret"));
    assert!(!snapshot.credential_is_ambient(STT_API_KEY_ENV, "owned-secret"));

    snapshot.set(STT_API_KEY_ENV, "   ").unwrap();
    assert_eq!(snapshot.value(STT_API_KEY_ENV), None);
}

#[test]
fn value_names_and_count_include_credentials_without_values() {
    let snapshot = RuntimeSettingsSnapshot::from_pairs([
        ("VOICEPI_LANG".to_owned(), "da".to_owned()),
        ("UNRECOGNIZED_COMPAT_VALUE".to_owned(), "kept".to_owned()),
        (GROQ_API_KEY_ENV.to_owned(), "groq-secret".to_owned()),
    ])
    .unwrap();

    assert_eq!(snapshot.value_count(), 3);
    assert_eq!(
        snapshot.value_names(),
        vec![
            GROQ_API_KEY_ENV.to_owned(),
            "UNRECOGNIZED_COMPAT_VALUE".to_owned(),
            "VOICEPI_LANG".to_owned(),
        ]
    );
    let debug = format!("{snapshot:?}");
    assert!(debug.contains(GROQ_API_KEY_ENV));
    assert!(!debug.contains("groq-secret"));
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
#[test]
fn owned_pairs_round_trip_credentials_for_in_process_resolution() {
    let snapshot = RuntimeSettingsSnapshot::from_pairs([
        ("VOICEPI_LANG".to_owned(), "da".to_owned()),
        (OPENAI_API_KEY_ENV.to_owned(), "openai-secret".to_owned()),
    ])
    .unwrap();

    let pairs = snapshot
        .pairs_owned()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(pairs["VOICEPI_LANG"], "da");
    assert_eq!(pairs[OPENAI_API_KEY_ENV], "openai-secret");
}
