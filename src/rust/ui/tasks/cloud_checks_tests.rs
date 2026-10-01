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
