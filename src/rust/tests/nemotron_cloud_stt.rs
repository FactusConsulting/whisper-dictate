//! Cross-OS Nemotron hosted Riva gRPC integration test.
//!
//! The test is deliberately opt-in: it runs only when `NEMOTRON_API_KEY` is
//! present.  That keeps fork PRs and ordinary local test runs deterministic,
//! while the dedicated workflow can exercise the real hosted endpoint with a
//! repository secret.  The key is read from the environment and is never put
//! in argv or test output.

use std::path::PathBuf;

use whisper_dictate_app::dictate::{
    CloudTranscribeBackend, CloudTranscribeConfig, TranscribeBackend,
};
use whisper_dictate_app::whisper::decode_wav_16k_mono;

const NEMOTRON_GRPC_ENDPOINT: &str = "https://grpc.nvcf.nvidia.com:443";
const NEMOTRON_ENGLISH_MODEL: &str = "nvidia/nemotron-speech-streaming-en-0.6b";

fn speech_wav_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello_speech.wav")
}

#[test]
fn nemotron_hosted_riva_transcribes_spoken_words() {
    let api_key = match std::env::var("NEMOTRON_API_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            eprintln!(concat!(
                "[nemotron-cloud-stt] NEMOTRON_API_KEY not set; skipping the live Nemotron ",
                "transcription test (fork PR / no secret)."
            ));
            return;
        }
    };

    let endpoint = std::env::var("NEMOTRON_GRPC_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| NEMOTRON_GRPC_ENDPOINT.to_owned());
    // NVIDIA's public Build function is currently the English deployment. A
    // multilingual function can be exercised by setting both
    // `NEMOTRON_MODEL` and an endpoint containing
    // `?function-id=<multilingual-function-id>` in the workflow/local shell.
    let model = std::env::var("NEMOTRON_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| NEMOTRON_ENGLISH_MODEL.to_owned());
    let language = std::env::var("NEMOTRON_LANGUAGE_CODE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "en-US".to_owned());
    let pcm =
        decode_wav_16k_mono(&speech_wav_path()).expect("decode bundled hello_speech.wav fixture");
    assert!(
        !pcm.is_empty(),
        "hello_speech.wav fixture produced no PCM samples"
    );

    let backend = CloudTranscribeBackend::new_with_provider(
        CloudTranscribeConfig {
            base_url: endpoint,
            api_key,
            model,
            timeout_ms: 60_000,
            // `auto` is the current request value for a multilingual
            // deployment. The public Build function defaults to en-US, while
            // a user-owned multi function can set NEMOTRON_LANGUAGE_CODE=auto.
            language: Some(language),
            prompt: None,
        },
        "nemotron",
    );

    let result = backend
        .transcribe(&pcm, 16_000)
        .expect("hosted Nemotron Riva transcription should succeed");
    let transcript = result.text.trim().to_lowercase();
    eprintln!(
        "[nemotron-cloud-stt] transcript={transcript:?} language={:?}",
        result.language
    );

    assert!(
        !transcript.is_empty(),
        "Nemotron returned an empty transcript for a spoken 'hello world' clip"
    );
    assert!(
        transcript.contains("hello") || transcript.contains("world"),
        "expected the spoken words in the transcript, got {transcript:?}"
    );
}
