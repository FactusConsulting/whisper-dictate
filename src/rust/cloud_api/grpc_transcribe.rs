//! Nemotron/Riva gRPC transcription client.
//!
//! The hosted Nemotron service is a Riva streaming endpoint, not an
//! OpenAI-compatible multipart endpoint.  Keep this adapter separate from
//! the API-check probe so the protocol messages and audio conversion stay
//! small and independently testable.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use http::uri::PathAndQuery;
use prost::Message;
use tonic::{
    client::Grpc,
    metadata::MetadataValue,
    transport::{ClientTlsConfig, Endpoint},
    Request,
};
use tonic_prost::ProstCodec;

use super::grpc::{authority_host, endpoint_url, function_id, remaining_timeout, NVCF_HOST};
use super::transcribe::CloudTranscriptionResult;

const STREAMING_RECOGNIZE_PATH: &str = "/nvidia.riva.asr.RivaSpeechRecognition/StreamingRecognize";
const LINEAR_PCM_ENCODING: i32 = 1;
// Riva's Python client defaults to 1,600 frames per request.  The in-process
// capture path is 16-bit PCM, so that is 3,200 bytes at the normal 16 kHz rate.
const AUDIO_CHUNK_BYTES: usize = 3_200;

fn transcription_timeout_error(endpoint: &str, timeout: Duration) -> anyhow::Error {
    anyhow!(
        "Nemotron gRPC transcription timed out after {} ms ({endpoint})",
        timeout.as_millis()
    )
}

/// Transcribe a 16-bit mono WAV through a Riva/NVCF Nemotron endpoint.
///
/// The OpenAI-compatible cloud API keeps the WAV container, while Riva wants
/// raw PCM chunks after a first configuration message.  The endpoint parser
/// also accepts NVIDIA's documented bare `grpc.nvcf.nvidia.com:443` spelling.
pub(crate) fn transcribe_nemotron_grpc(
    base_url: &str,
    api_key: &str,
    model: &str,
    audio_wav: &[u8],
    language: Option<&str>,
    prompt: Option<&str>,
    timeout_ms: u64,
) -> Result<CloudTranscriptionResult> {
    let (audio, sample_rate) = decode_wav(audio_wav)?;
    let timeout = Duration::from_millis(timeout_ms.max(1_000));
    let (endpoint_url, tls) = endpoint_url(base_url)?;
    if authority_host(base_url).as_deref() == Some(NVCF_HOST) && !tls {
        return Err(anyhow!(
            "hosted Nemotron gRPC endpoint requires a TLS https:// URL"
        ));
    }
    let function_id = function_id(base_url);
    let api_key = api_key.trim().to_owned();
    let configured_model = model.trim();
    if configured_model.is_empty() {
        return Err(anyhow!("Nemotron gRPC model is empty"));
    }
    let model = riva_model_name(base_url, configured_model);
    let endpoint_host = endpoint_url.clone();
    let language = riva_language_code(language);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not create the Nemotron gRPC runtime")?;
    runtime.block_on(async move {
        let deadline = tokio::time::Instant::now() + timeout;
        let endpoint = Endpoint::from_shared(endpoint_url.clone())
            .map_err(|err| anyhow!("invalid Nemotron gRPC endpoint: {err}"))?
            .connect_timeout(timeout);
        let endpoint = if tls {
            endpoint
                .tls_config(ClientTlsConfig::new().with_enabled_roots())
                .map_err(|err| anyhow!("could not configure Nemotron gRPC TLS: {err}"))?
        } else {
            endpoint
        };
        let channel = tokio::time::timeout(remaining_timeout(deadline), endpoint.connect())
            .await
            .map_err(|_| transcription_timeout_error(&endpoint_host, timeout))?
            .map_err(|err| anyhow!("Nemotron gRPC connection failed: {err}"))?;

        let mut request_messages = Vec::with_capacity(1 + audio.len().div_ceil(AUDIO_CHUNK_BYTES));
        request_messages.push(StreamingRecognizeRequest {
            streaming_request: Some(
                streaming_recognize_request::StreamingRequest::StreamingConfig(
                    StreamingRecognitionConfig {
                        config: Some(recognition_config(sample_rate, language, model, prompt)),
                        interim_results: true,
                    },
                ),
            ),
        });
        request_messages.extend(audio.chunks(AUDIO_CHUNK_BYTES).map(|chunk| {
            StreamingRecognizeRequest {
                streaming_request: Some(
                    streaming_recognize_request::StreamingRequest::AudioContent(chunk.to_vec()),
                ),
            }
        }));

        let mut request = Request::new(tokio_stream::iter(request_messages));
        if !api_key.is_empty() {
            let value = MetadataValue::try_from(format!("Bearer {api_key}"))
                .map_err(|_| anyhow!("Nemotron gRPC API key could not be encoded"))?;
            request.metadata_mut().insert("authorization", value);
        }
        if let Some(function_id) = function_id {
            let value = MetadataValue::try_from(function_id.as_str())
                .map_err(|_| anyhow!("Nemotron gRPC function id could not be encoded"))?;
            request.metadata_mut().insert("function-id", value);
        }

        let mut grpc = Grpc::new(channel);
        // `Grpc::streaming` calls the underlying tower service directly.  The
        // explicit readiness poll is required by tower::buffer and prevents
        // the `send_item called without first calling poll_reserve` panic that
        // otherwise kills the UI's background API-check/runtime thread.
        tokio::time::timeout(remaining_timeout(deadline), grpc.ready())
            .await
            .map_err(|_| transcription_timeout_error(&endpoint_host, timeout))?
            .map_err(|err| anyhow!("Nemotron gRPC transcription failed: {err}"))?;
        let response = tokio::time::timeout(
            remaining_timeout(deadline),
            grpc.streaming(
                request,
                PathAndQuery::from_static(STREAMING_RECOGNIZE_PATH),
                ProstCodec::<StreamingRecognizeRequest, StreamingRecognizeResponse>::default(),
            ),
        )
        .await
        .map_err(|_| transcription_timeout_error(&endpoint_host, timeout))?
        .map_err(|status| anyhow!("Nemotron gRPC transcription failed: {status}"))?;

        let mut stream = response.into_inner();
        let mut final_text = String::new();
        let mut latest_text = String::new();
        let mut detected_language = None;
        while let Some(response) =
            tokio::time::timeout(remaining_timeout(deadline), stream.message())
                .await
                .map_err(|_| transcription_timeout_error(&endpoint_host, timeout))?
                .map_err(|status| anyhow!("Nemotron gRPC transcription failed: {status}"))?
        {
            for result in response.results {
                let Some(alternative) = result.alternatives.first() else {
                    continue;
                };
                let text = alternative.transcript.trim();
                if text.is_empty() {
                    continue;
                }
                latest_text = text.to_owned();
                if result.is_final {
                    append_final_segment(&mut final_text, text);
                }
                if detected_language.is_none() {
                    detected_language = alternative
                        .language_code
                        .iter()
                        .map(String::as_str)
                        .map(str::trim)
                        .find(|value| !value.is_empty())
                        .map(str::to_owned);
                }
            }
        }

        Ok(CloudTranscriptionResult {
            text: if final_text.is_empty() {
                latest_text
            } else {
                final_text
            },
            language: detected_language,
        })
    })
}

fn decode_wav(audio_wav: &[u8]) -> Result<(Vec<u8>, u32)> {
    let mut reader = hound::WavReader::new(Cursor::new(audio_wav))
        .context("Nemotron gRPC audio is not a valid WAV")?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(anyhow!("Nemotron gRPC audio must be mono"));
    }
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample != 16 {
        return Err(anyhow!("Nemotron gRPC audio must be 16-bit PCM"));
    }
    if spec.sample_rate == 0 {
        return Err(anyhow!("Nemotron gRPC audio sample rate is zero"));
    }
    let mut pcm = Vec::with_capacity(reader.duration() as usize * 2);
    for sample in reader.samples::<i16>() {
        pcm.extend_from_slice(
            &sample
                .context("Nemotron gRPC WAV sample is invalid")?
                .to_le_bytes(),
        );
    }
    if pcm.is_empty() {
        return Err(anyhow!("Nemotron gRPC audio is empty"));
    }
    Ok((pcm, spec.sample_rate))
}

fn riva_model_name(base_url: &str, configured_model: &str) -> String {
    // NVIDIA's hosted Build function already selects the profile identified by
    // `function-id`; its documented client leaves `model` empty. Sending the
    // UI's NIM model id (which contains the `nvidia/` namespace) can otherwise
    // be rejected as an unknown Riva model name. Self-hosted Riva endpoints
    // retain the configured model so multi-model servers can still select it.
    if authority_host(base_url).as_deref() == Some(NVCF_HOST) {
        String::new()
    } else {
        configured_model.to_owned()
    }
}

fn append_final_segment(output: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let starts_with_attached_punctuation = text.chars().next().is_some_and(|character| {
        matches!(
            character,
            ',' | '.' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '%' | '\'' | '"'
        )
    });
    if !output.is_empty() && !starts_with_attached_punctuation {
        output.push(' ');
    }
    output.push_str(text);
}

fn recognition_config(
    sample_rate: u32,
    language: String,
    model: String,
    prompt: Option<&str>,
) -> RecognitionConfig {
    RecognitionConfig {
        encoding: LINEAR_PCM_ENCODING,
        sample_rate_hertz: i32::try_from(sample_rate).unwrap_or(i32::MAX),
        language_code: language,
        max_alternatives: 1,
        speech_contexts: riva_speech_contexts(prompt),
        enable_automatic_punctuation: true,
        model,
    }
}

fn riva_speech_contexts(prompt: Option<&str>) -> Vec<SpeechContext> {
    let Some(prompt) = prompt.map(str::trim).filter(|value| !value.is_empty()) else {
        return Vec::new();
    };
    let mut phrases = Vec::new();
    for line in prompt
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(vocabulary) = line.strip_prefix("Vocabulary:") {
            phrases.extend(
                vocabulary
                    .split(',')
                    .map(str::trim)
                    .filter(|phrase| !phrase.is_empty())
                    .map(str::to_owned),
            );
        } else {
            phrases.push(line.to_owned());
        }
    }
    if phrases.is_empty() {
        Vec::new()
    } else {
        vec![SpeechContext {
            phrases,
            boost: 10.0,
        }]
    }
}

fn riva_language_code(language: Option<&str>) -> String {
    // `multi` is the HTTP/NIM selector used by older callers. Riva's
    // multilingual Nemotron profile detects the language when this field is
    // omitted, so do not send the selector as a BCP-47 language code.
    language
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && !value.eq_ignore_ascii_case("auto")
                && !value.eq_ignore_ascii_case("multi")
        })
        .unwrap_or_default()
        .to_owned()
}

#[derive(Clone, PartialEq, Message)]
struct StreamingRecognizeRequest {
    #[prost(oneof = "streaming_recognize_request::StreamingRequest", tags = "1, 2")]
    streaming_request: Option<streaming_recognize_request::StreamingRequest>,
}

mod streaming_recognize_request {
    use super::StreamingRecognitionConfig;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum StreamingRequest {
        #[prost(message, tag = "1")]
        StreamingConfig(StreamingRecognitionConfig),
        #[prost(bytes, tag = "2")]
        AudioContent(Vec<u8>),
    }
}

#[derive(Clone, PartialEq, Message)]
struct StreamingRecognitionConfig {
    #[prost(message, optional, tag = "1")]
    config: Option<RecognitionConfig>,
    #[prost(bool, tag = "2")]
    interim_results: bool,
}

#[derive(Clone, PartialEq, Message)]
struct RecognitionConfig {
    #[prost(int32, tag = "1")]
    encoding: i32,
    #[prost(int32, tag = "2")]
    sample_rate_hertz: i32,
    #[prost(string, tag = "3")]
    language_code: String,
    #[prost(int32, tag = "4")]
    max_alternatives: i32,
    #[prost(message, repeated, tag = "6")]
    speech_contexts: Vec<SpeechContext>,
    #[prost(bool, tag = "11")]
    enable_automatic_punctuation: bool,
    #[prost(string, tag = "13")]
    model: String,
}

#[derive(Clone, PartialEq, Message)]
struct SpeechContext {
    #[prost(string, repeated, tag = "1")]
    phrases: Vec<String>,
    #[prost(float, tag = "4")]
    boost: f32,
}

#[derive(Clone, PartialEq, Message)]
struct StreamingRecognizeResponse {
    #[prost(message, repeated, tag = "1")]
    results: Vec<StreamingRecognitionResult>,
}

#[derive(Clone, PartialEq, Message)]
struct StreamingRecognitionResult {
    #[prost(message, repeated, tag = "1")]
    alternatives: Vec<SpeechRecognitionAlternative>,
    #[prost(bool, tag = "2")]
    is_final: bool,
}

#[derive(Clone, PartialEq, Message)]
struct SpeechRecognitionAlternative {
    #[prost(string, tag = "1")]
    transcript: String,
    #[prost(string, repeated, tag = "4")]
    language_code: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn auto_language_is_sent_as_empty_riva_language_code() {
        assert_eq!(riva_language_code(Some("auto")), "");
        assert_eq!(riva_language_code(Some(" Auto ")), "");
        assert_eq!(riva_language_code(Some("multi")), "");
        assert_eq!(riva_language_code(Some(" en-US ")), "en-US");
        assert_eq!(riva_language_code(None), "");
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
    fn final_segments_keep_word_boundaries_and_attach_punctuation() {
        let mut text = String::new();
        append_final_segment(&mut text, "hello");
        append_final_segment(&mut text, "world");
        append_final_segment(&mut text, "!");
        assert_eq!(text, "hello world!");
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
    }

    #[test]
    fn provider_constant_matches_grpc_probe_label() {
        assert_eq!(NEMOTRON_PROVIDER, "nemotron 3.5 asr");
    }
}
