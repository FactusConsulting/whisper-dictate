//! Nemotron-specific cloud API checks.
//!
//! The provider uses Riva gRPC rather than the OpenAI-compatible `/models`
//! endpoint. Keeping its fixture probe and model alias rules here prevents the
//! generic cloud check from accumulating provider-specific branches.

use anyhow::Result;

use super::{
    check::{CloudApiCheck, CloudApiCheckResult},
    grpc_transcribe,
};

pub(crate) fn check_cloud_api(check: &CloudApiCheck) -> Result<CloudApiCheckResult> {
    // Use a real streaming request instead of GetRivaSpeechRecognitionConfig:
    // NVCF functions can report a healthy service while rejecting the
    // selected profile's language code or model at transcription time. The
    // fixture is tiny and deterministic; a blank transcript still proves TLS,
    // credentials, function selection, protobuf encoding, and the
    // model/language contract were accepted.
    let result = grpc_transcribe::transcribe_nemotron_grpc(
        &check.base_url,
        &check.api_key,
        &check.model,
        include_bytes!("../tests/fixtures/hello_speech.wav"),
        check.language.as_deref(),
        None,
        check.timeout_ms,
    )?;
    Ok(CloudApiCheckResult {
        provider: check.provider.clone(),
        model: check.model.clone(),
        model_count: 1,
        model_available: true,
        probe_text: Some(result.text),
        probe_language: result.language,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelProfile {
    Generic,
    English,
    Multilingual,
}

fn model_profile(model: &str) -> Option<ModelProfile> {
    match model {
        "nemotron-asr-streaming" | "nvidia/nemotron-asr-streaming" => Some(ModelProfile::Generic),
        "nemotron-speech-streaming-en-0.6b" | "nvidia/nemotron-speech-streaming-en-0.6b" => {
            Some(ModelProfile::English)
        }
        "nemotron-3.5-asr-streaming-0.6b" | "nvidia/nemotron-3.5-asr-streaming-0.6b" => {
            Some(ModelProfile::Multilingual)
        }
        _ => None,
    }
}

pub(crate) fn model_id_matches(expected: &str, advertised: &str) -> bool {
    let expected = expected.trim();
    let advertised = advertised.trim();
    if expected == advertised {
        return true;
    }
    // Ordinary OpenAI-compatible providers may treat model ids as
    // case-sensitive. Only documented Nemotron aliases are normalized; a
    // casing typo for any other model must remain unavailable.
    let expected_normalized = expected.to_ascii_lowercase();
    let advertised_normalized = advertised.to_ascii_lowercase();
    matches!(
        (
            model_profile(&expected_normalized),
            model_profile(&advertised_normalized)
        ),
        (Some(ModelProfile::Generic), Some(_))
            | (Some(_), Some(ModelProfile::Generic))
            | (Some(ModelProfile::English), Some(ModelProfile::English))
            | (
                Some(ModelProfile::Multilingual),
                Some(ModelProfile::Multilingual)
            )
    )
}

#[cfg(test)]
mod tests {
    use super::model_id_matches;

    #[test]
    fn model_matching_accepts_hosted_nemotron_aliases() {
        assert!(model_id_matches(
            "nvidia/nemotron-3.5-asr-streaming-0.6b",
            "nemotron-asr-streaming"
        ));
        assert!(!model_id_matches(
            "whisper-large-v3",
            "nemotron-asr-streaming"
        ));
        assert!(!model_id_matches(
            "nvidia/nemotron-3.5-asr-streaming-0.6b",
            "nemotron-4-asr-streaming"
        ));
        assert!(!model_id_matches(
            "tenant/nemotron-3.5-asr-streaming-0.6b",
            "other/nemotron-3.5-asr-streaming-0.6b"
        ));
        assert!(model_id_matches(
            "nvidia/nemotron-speech-streaming-en-0.6b",
            "nvidia/nemotron-speech-streaming-en-0.6b"
        ));
        assert!(!model_id_matches(
            "nvidia/nemotron-speech-streaming-en-0.6b",
            "nvidia/nemotron-3.5-asr-streaming-0.6b"
        ));
        assert!(!model_id_matches(
            "GPT-4O-MINI-TRANSCRIBE",
            "gpt-4o-mini-transcribe"
        ));
        assert!(model_id_matches(
            "nvidia/nemotron-speech-streaming-en-0.6b",
            "nemotron-asr-streaming"
        ));
    }
}
