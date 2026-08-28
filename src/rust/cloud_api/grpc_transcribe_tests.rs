use std::io::Cursor;
use std::time::Duration;

use prost::Message;

use super::{
    append_result, decode_wav, recognition_config, riva_language_code, riva_model_name,
    streaming_recognize_request, transcription_timeout_error, SpeechRecognitionAlternative,
    StreamingRecognitionResult, StreamingRecognizeRequest, StreamingRecognizeResponse,
};
use crate::cloud_api::grpc::NEMOTRON_PROVIDER;

#[test]
fn decode_wav_returns_raw_little_endian_pcm() {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut bytes = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut bytes, spec).unwrap();
        writer.write_sample(0x1234_i16).unwrap();
        writer.write_sample(-2_i16).unwrap();
        writer.finalize().unwrap();
    }
    let (pcm, sample_rate) = decode_wav(bytes.get_ref()).unwrap();
    assert_eq!(sample_rate, 16_000);
    assert_eq!(pcm, [0x34, 0x12, 0xfe, 0xff]);
}

#[test]
fn streaming_request_uses_riva_oneof_tags() {
    let request = StreamingRecognizeRequest {
        streaming_request: Some(streaming_recognize_request::StreamingRequest::AudioContent(
            vec![1, 2, 3],
        )),
    };
    let mut encoded = Vec::new();
    request.encode(&mut encoded).unwrap();
    assert_eq!(encoded, [0x12, 0x03, 0x01, 0x02, 0x03]);
}

#[test]
fn auto_language_is_sent_as_the_current_riva_auto_code() {
    assert_eq!(riva_language_code(Some("auto")), "auto");
    assert_eq!(riva_language_code(Some(" Auto ")), "auto");
    assert_eq!(riva_language_code(Some("multi")), "auto");
    assert_eq!(riva_language_code(Some(" Multi ")), "auto");
    assert_eq!(riva_language_code(Some(" en-US ")), "en-US");
    assert_eq!(riva_language_code(None), "auto");
}

#[test]
fn compact_language_hints_are_expanded_to_nemotron_locales() {
    assert_eq!(riva_language_code(Some("ar")), "ar-AR");
    assert_eq!(riva_language_code(Some("en")), "en-US");
    assert_eq!(riva_language_code(Some("da")), "da-DK");
    assert_eq!(riva_language_code(Some("de")), "de-DE");
    assert_eq!(riva_language_code(Some("fr")), "fr-FR");
    assert_eq!(riva_language_code(Some("nb")), "nb-NO");
}

#[test]
fn regional_language_hints_are_canonicalized_without_changing_locale() {
    assert_eq!(riva_language_code(Some(" EN_us ")), "en-US");
    assert_eq!(riva_language_code(Some("da_dk")), "da-DK");
    assert_eq!(riva_language_code(Some("fr-CA")), "fr-CA");
}

#[test]
fn prompt_is_forwarded_as_riva_speech_contexts() {
    let config = recognition_config(
        16_000,
        String::new(),
        "nemotron".to_owned(),
        Some("Use technical terms\nVocabulary: Kubernetes, Cloud Code"),
    );
    assert_eq!(
        config.speech_contexts[0].phrases,
        ["Use technical terms", "Kubernetes", "Cloud Code"]
    );
    assert_eq!(config.speech_contexts[0].boost, 10.0);
}

#[test]
fn final_language_replaces_an_interim_hypothesis() {
    let mut final_text = String::new();
    let mut latest_text = String::new();
    let mut language = None;
    append_result(
        &mut final_text,
        &mut latest_text,
        &mut language,
        &StreamingRecognitionResult {
            alternatives: vec![SpeechRecognitionAlternative {
                transcript: "hello".to_owned(),
                language_code: vec!["en-US".to_owned()],
            }],
            is_final: false,
        },
    );
    append_result(
        &mut final_text,
        &mut latest_text,
        &mut language,
        &StreamingRecognitionResult {
            alternatives: vec![SpeechRecognitionAlternative {
                transcript: "bonjour".to_owned(),
                language_code: vec!["fr-FR".to_owned()],
            }],
            is_final: true,
        },
    );
    assert_eq!(language.as_deref(), Some("fr-FR"));
}

#[test]
fn service_response_wire_bytes_decode_language_from_alternative_field_four() {
    // This is the wire shape emitted by Riva: result field 1 contains an
    // alternative whose language_code is field 4. Field 5 on the result is
    // reserved for channel_tag in the public Riva schema.
    let bytes = [
        0x0a, 0x12, // response.results (length 18)
        0x0a, 0x0e, // result.alternatives (length 14)
        0x0a, 0x05, b'h', b'e', b'l', b'l', b'o', // transcript
        0x22, 0x05, b'e', b'n', b'-', b'U', b'S', // language_code
        0x10, 0x01, // result.is_final
    ];
    let response = StreamingRecognizeResponse::decode(bytes.as_slice()).unwrap();
    let mut final_text = String::new();
    let mut latest_text = String::new();
    let mut language = None;
    append_result(
        &mut final_text,
        &mut latest_text,
        &mut language,
        &response.results[0],
    );

    assert_eq!(final_text, "hello");
    assert_eq!(language.as_deref(), Some("en-US"));
}

#[test]
fn transcription_timeout_is_not_labeled_as_an_api_check() {
    let message =
        transcription_timeout_error("https://grpc.nvcf.nvidia.com:443", Duration::from_secs(2));
    assert!(message.to_string().contains("transcription timed out"));
    assert!(!message.to_string().contains("API check"));
}

#[test]
fn hosted_riva_uses_function_selected_model() {
    assert_eq!(
        riva_model_name(
            "https://grpc.nvcf.nvidia.com:443",
            "nvidia/nemotron-3.5-asr-streaming-0.6b"
        ),
        ""
    );
    assert_eq!(
        riva_model_name("grpc://localhost:50051", "local-model"),
        "local-model"
    );
    assert_eq!(
        riva_model_name(
            "grpc://localhost:50051",
            "nvidia/nemotron-3.5-asr-streaming-0.6b"
        ),
        "nemotron-asr-streaming"
    );
    assert_eq!(
        riva_model_name(
            "grpc://localhost:50051",
            "nvidia/nemotron-speech-streaming-en-0.6b"
        ),
        "nemotron-asr-streaming"
    );
}

#[test]
fn provider_constant_matches_grpc_probe_label() {
    assert_eq!(NEMOTRON_PROVIDER, "nemotron 3.5 asr");
}
