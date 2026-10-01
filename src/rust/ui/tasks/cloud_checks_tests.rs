use super::*;

#[test]
fn cloud_api_check_panic_is_reported_on_the_background_result_channel() {
    let check = CloudApiCheck {
        provider: "Nemotron 3.5 ASR".to_owned(),
        base_url: "grpc.nvcf.nvidia.com:443".to_owned(),
        model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
        api_key: "test-key".to_owned(),
        language: None,
        device: "auto".to_owned(),
        local_only: false,
        timeout_ms: 1_000,
    };

    let result = super::cloud_api_check_result(
        &check,
        "Nemotron gRPC API check".to_owned(),
        |_check| -> anyhow::Result<CloudApiCheckResult> {
            panic!("simulated gRPC transport panic")
        },
    );

    assert_eq!(result.label, "cloud API check");
    assert_eq!(result.command, "Nemotron gRPC API check");
    assert!(!result.success);
    assert!(result
        .error
        .as_deref()
        .is_some_and(|message| message.contains("cloud API check panicked")
            && message.contains("simulated gRPC transport panic")));
}

fn fixture_check() -> CloudApiCheck {
    CloudApiCheck {
        provider: "custom".to_owned(),
        base_url: "http://127.0.0.1:12345".to_owned(),
        model: "fixture-model".to_owned(),
        api_key: String::new(),
        language: None,
        device: "auto".to_owned(),
        local_only: false,
        timeout_ms: 1000,
    }
}

#[test]
fn cloud_check_success_and_missing_model_preserve_the_operation_and_summary() {
    for available in [false, true] {
        let check = fixture_check();
        let expected = CloudApiCheckResult {
            provider: check.provider.clone(),
            model: check.model.clone(),
            model_count: 2,
            model_available: available,
            probe_text: None,
            probe_language: None,
        };
        let summary = expected.summary();
        let result =
            super::cloud_api_check_result(&check, "fixture operation".to_owned(), |_| Ok(expected));
        assert_eq!(result.label, "cloud API check");
        assert_eq!(result.command, "fixture operation");
        assert_eq!(result.stdout, summary);
        assert_eq!(result.success, available);
        assert!(result.error.is_none());
        assert!(result.stderr.is_empty());
    }
}

#[test]
fn cloud_check_error_is_carried_by_the_result_envelope() {
    let result =
        super::cloud_api_check_result(&fixture_check(), "fixture operation".to_owned(), |_| {
            Err(anyhow::anyhow!("fixture transport failed"))
        });
    assert!(!result.success);
    assert_eq!(result.error.as_deref(), Some("fixture transport failed"));
    assert_eq!(result.command, "fixture operation");
    assert!(result.stdout.is_empty());
    assert!(result.stderr.is_empty());
}
