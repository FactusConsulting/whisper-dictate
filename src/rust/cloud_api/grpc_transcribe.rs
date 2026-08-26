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
    transport::{Channel, ClientTlsConfig, Endpoint},
    Request,
};
use tonic_prost::ProstCodec;

use super::grpc::{authority_host, endpoint_url, function_id, remaining_timeout, NVCF_HOST};
use super::transcribe::CloudTranscriptionResult;

#[path = "grpc_transcribe_text.rs"]
mod text;
use text::append_final_segment;

const STREAMING_RECOGNIZE_PATH: &str = "/nvidia.riva.asr.RivaSpeechRecognition/StreamingRecognize";
const LINEAR_PCM_ENCODING: i32 = 1;
// Riva's Python client defaults to 1,600 frames per request.  The in-process
// capture path is 16-bit PCM, so that is 3,200 bytes at the normal 16 kHz rate.
const AUDIO_CHUNK_BYTES: usize = 3_200;

struct GrpcTranscriptionConfig {
    audio: Vec<u8>,
    sample_rate: u32,
    endpoint_url: String,
    endpoint_host: String,
    tls: bool,
    function_id: Option<String>,
    api_key: String,
    model: String,
    language: String,
    prompt: Option<String>,
    timeout: Duration,
}

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
    let config = prepare_transcription_config(
        base_url, api_key, model, audio_wav, language, prompt, timeout_ms,
    )?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not create the Nemotron gRPC runtime")?;
    runtime.block_on(transcribe_stream(config))
}

fn prepare_transcription_config(
    base_url: &str,
    api_key: &str,
    model: &str,
    audio_wav: &[u8],
    language: Option<&str>,
    prompt: Option<&str>,
    timeout_ms: u64,
) -> Result<GrpcTranscriptionConfig> {
    let (audio, sample_rate) = decode_wav(audio_wav)?;
    let timeout = Duration::from_millis(timeout_ms.max(1_000));
    let (endpoint_url, tls) = endpoint_url(base_url)?;
    if authority_host(base_url).as_deref() == Some(NVCF_HOST) && !tls {
        return Err(anyhow!(
            "hosted Nemotron gRPC endpoint requires a TLS https:// URL"
        ));
    }
    let configured_model = model.trim();
    if configured_model.is_empty() {
        return Err(anyhow!("Nemotron gRPC model is empty"));
    }
    Ok(GrpcTranscriptionConfig {
        audio,
        sample_rate,
        endpoint_host: endpoint_url.clone(),
        endpoint_url,
        tls,
        function_id: function_id(base_url),
        api_key: api_key.trim().to_owned(),
        model: riva_model_name(base_url, configured_model),
        language: riva_language_code(language),
        prompt: prompt.map(str::to_owned),
        timeout,
    })
}

async fn transcribe_stream(config: GrpcTranscriptionConfig) -> Result<CloudTranscriptionResult> {
    let deadline = tokio::time::Instant::now() + config.timeout;
    let channel = connect_channel(&config, deadline).await?;
    let mut request = Request::new(tokio_stream::iter(build_request_messages(&config)));
    attach_metadata(&mut request, &config.api_key, config.function_id.as_deref())?;

    let mut grpc = Grpc::new(channel);
    // `Grpc::streaming` calls the underlying tower service directly.  The
    // explicit readiness poll is required by tower::buffer and prevents
    // the `send_item called without first calling poll_reserve` panic that
    // otherwise kills the UI's background API-check/runtime thread.
    tokio::time::timeout(remaining_timeout(deadline), grpc.ready())
        .await
        .map_err(|_| transcription_timeout_error(&config.endpoint_host, config.timeout))?
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
    .map_err(|_| transcription_timeout_error(&config.endpoint_host, config.timeout))?
    .map_err(|status| anyhow!("Nemotron gRPC transcription failed: {status}"))?;

    collect_transcription(response.into_inner(), &config, deadline).await
}

async fn connect_channel(
    config: &GrpcTranscriptionConfig,
    deadline: tokio::time::Instant,
) -> Result<Channel> {
    let endpoint = Endpoint::from_shared(config.endpoint_url.clone())
        .map_err(|err| anyhow!("invalid Nemotron gRPC endpoint: {err}"))?
        .connect_timeout(config.timeout);
    let endpoint = if config.tls {
        endpoint
            .tls_config(ClientTlsConfig::new().with_enabled_roots())
            .map_err(|err| anyhow!("could not configure Nemotron gRPC TLS: {err}"))?
    } else {
        endpoint
    };
    tokio::time::timeout(remaining_timeout(deadline), endpoint.connect())
        .await
        .map_err(|_| transcription_timeout_error(&config.endpoint_host, config.timeout))?
        .map_err(|err| anyhow!("Nemotron gRPC connection failed: {err}"))
}

fn build_request_messages(config: &GrpcTranscriptionConfig) -> Vec<StreamingRecognizeRequest> {
    let mut messages = Vec::with_capacity(1 + config.audio.len().div_ceil(AUDIO_CHUNK_BYTES));
    messages.push(StreamingRecognizeRequest {
        streaming_request: Some(
            streaming_recognize_request::StreamingRequest::StreamingConfig(
                StreamingRecognitionConfig {
                    config: Some(recognition_config(
                        config.sample_rate,
                        config.language.clone(),
                        config.model.clone(),
                        config.prompt.as_deref(),
                    )),
                    interim_results: true,
                },
            ),
        ),
    });
    messages.extend(config.audio.chunks(AUDIO_CHUNK_BYTES).map(|chunk| {
        StreamingRecognizeRequest {
            streaming_request: Some(streaming_recognize_request::StreamingRequest::AudioContent(
                chunk.to_vec(),
            )),
        }
    }));
    messages
}

fn attach_metadata<T>(
    request: &mut Request<T>,
    api_key: &str,
    function_id: Option<&str>,
) -> Result<()> {
    if !api_key.is_empty() {
        let value = MetadataValue::try_from(format!("Bearer {api_key}"))
            .map_err(|_| anyhow!("Nemotron gRPC API key could not be encoded"))?;
        request.metadata_mut().insert("authorization", value);
    }
    if let Some(function_id) = function_id {
        let value = MetadataValue::try_from(function_id)
            .map_err(|_| anyhow!("Nemotron gRPC function id could not be encoded"))?;
        request.metadata_mut().insert("function-id", value);
    }
    Ok(())
}

async fn collect_transcription(
    mut stream: tonic::codec::Streaming<StreamingRecognizeResponse>,
    config: &GrpcTranscriptionConfig,
    deadline: tokio::time::Instant,
) -> Result<CloudTranscriptionResult> {
    let mut final_text = String::new();
    let mut latest_text = String::new();
    let mut detected_language = None;
    while let Some(response) = tokio::time::timeout(remaining_timeout(deadline), stream.message())
        .await
        .map_err(|_| transcription_timeout_error(&config.endpoint_host, config.timeout))?
        .map_err(|status| anyhow!("Nemotron gRPC transcription failed: {status}"))?
    {
        for result in response.results {
            append_result(
                &mut final_text,
                &mut latest_text,
                &mut detected_language,
                &result,
            );
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
}

fn append_result(
    final_text: &mut String,
    latest_text: &mut String,
    detected_language: &mut Option<String>,
    result: &StreamingRecognitionResult,
) {
    let Some(alternative) = result.alternatives.first() else {
        return;
    };
    let text = alternative.transcript.trim();
    if text.is_empty() {
        return;
    }
    *latest_text = text.to_owned();
    if result.is_final {
        append_final_segment(final_text, text);
    }
    let language = alternative
        .language_code
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned);
    if result.is_final {
        // Interim hypotheses can be revised by the final decoder result. Keep
        // the useful interim fallback, but always let a final language win.
        if language.is_some() {
            *detected_language = language;
        }
    } else if detected_language.is_none() {
        *detected_language = language;
    }
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
    } else if is_nemotron_model_alias(configured_model) {
        // Local Riva/NIM servers commonly advertise the short service name
        // (`nemotron-asr-streaming`) rather than the UI's Hugging Face-style
        // model id. Use that advertised selector so StreamingRecognize does
        // not fail even though the API-check alias matcher accepted the UI id.
        "nemotron-asr-streaming".to_owned()
    } else {
        configured_model.to_owned()
    }
}

fn is_nemotron_model_alias(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "nemotron-asr-streaming"
            | "nemotron-3.5-asr-streaming-0.6b"
            | "nvidia/nemotron-asr-streaming"
            | "nvidia/nemotron-3.5-asr-streaming-0.6b"
    )
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
#[path = "grpc_transcribe_tests.rs"]
mod tests;
