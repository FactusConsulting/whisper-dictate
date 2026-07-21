//! Cross-OS Groq cloud-STT integration test.
//!
//! Exercises the REAL [`CloudTranscribeBackend`] end-to-end against Groq's
//! `/audio/transcriptions` endpoint using the bundled `hello_speech.wav`:
//! decode the fixture to PCM, run it through the session's cloud transcribe
//! backend (which re-encodes to WAV and POSTs it), and assert Groq returns
//! the actual spoken words.
//!
//! Runs on ubuntu AND windows via `cargo test` -- Groq performs the STT
//! over HTTP, so no local Whisper model, GPU, or microphone is needed, and
//! the same test binary behaves identically on both desktops.
//!
//! **What it asserts.** `hello_speech.wav` is a machine-synthesized
//! ("Hello world.", via `espeak-ng` -- no real person's voice; see
//! `tests/fixtures/README.md`) 16 kHz mono clip, so unlike the synthetic
//! `hello.wav` tone it carries real, transcribable speech. The test
//! asserts a non-empty transcript that contains the spoken words -- i.e.
//! the full pipeline (auth + URL + model + WAV upload + fixture decode)
//! AND that Groq genuinely transcribed the audio, not merely that the HTTP
//! call succeeded.
//!
//! **Self-skipping.** With `GROQ_API_KEY` unset (fork PRs, local runs
//! without a key) it prints a notice and passes, so it is harmless in the
//! ordinary `rust` matrix. A dedicated, NON-required workflow runs it WITH
//! the `GROQ_API_KEY` secret on both desktops -- a Groq/network hiccup must
//! never gate a merge.

use std::path::PathBuf;

use whisper_dictate_app::dictate::{
    CloudTranscribeBackend, CloudTranscribeConfig, TranscribeBackend,
};
use whisper_dictate_app::whisper::decode_wav_16k_mono;

const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
const GROQ_STT_MODEL: &str = "whisper-large-v3-turbo";

/// Path to the bundled speech fixture. `CARGO_MANIFEST_DIR` is `src/rust`.
fn speech_wav_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello_speech.wav")
}

#[test]
fn groq_cloud_stt_transcribes_spoken_words() {
    let api_key = match std::env::var("GROQ_API_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            eprintln!(
                "[groq-cloud-stt] GROQ_API_KEY not set; skipping the live Groq \
                 transcription test (fork PR / no secret)."
            );
            return;
        }
    };

    let pcm =
        decode_wav_16k_mono(&speech_wav_path()).expect("decode bundled hello_speech.wav fixture");
    assert!(
        !pcm.is_empty(),
        "hello_speech.wav fixture produced no PCM samples"
    );

    let backend = CloudTranscribeBackend::new(CloudTranscribeConfig {
        base_url: GROQ_BASE_URL.to_owned(),
        api_key,
        model: GROQ_STT_MODEL.to_owned(),
        timeout_ms: 30_000,
        language: Some("en".to_owned()),
        prompt: None,
    });

    let result = backend
        .transcribe(&pcm, 16_000)
        .expect("Groq cloud transcription should succeed for the hello_speech.wav clip");
    let transcript = result.text.trim().to_lowercase();
    eprintln!("[groq-cloud-stt] transcript: {transcript:?}");

    // Real speech -> a real transcript. This is the meaningful upgrade over
    // the synthetic tone (which legitimately returns empty text): Groq must
    // return the spoken words, proving end-to-end STT, not just HTTP wiring.
    assert!(
        !transcript.is_empty(),
        "Groq returned an empty transcript for a spoken 'hello world' clip"
    );
    assert!(
        transcript.contains("hello") || transcript.contains("world"),
        "expected the spoken words in the transcript, got {transcript:?}"
    );
}
