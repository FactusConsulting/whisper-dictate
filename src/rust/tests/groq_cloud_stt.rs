//! Cross-OS Groq cloud-STT integration test.
//!
//! Exercises the REAL [`CloudTranscribeBackend`] end-to-end against Groq's
//! `/audio/transcriptions` endpoint using the bundled 0.5 s `hello.wav`:
//! decode the fixture to PCM, run it through the session's cloud transcribe
//! backend (which re-encodes to WAV and POSTs it), and assert a successful
//! structured round trip.
//!
//! Runs on ubuntu AND windows via `cargo test` -- Groq performs the STT
//! over HTTP, so no local Whisper model, GPU, or microphone is needed, and
//! the same test binary behaves identically on both desktops.
//!
//! **What it asserts.** `hello.wav` is a synthetic 440 Hz amplitude-
//! modulated sine tone (see `src/python/tests/fixtures/README.md`), NOT a
//! spoken word, so its transcript is deliberately nondeterministic -- Groq
//! may legitimately return empty text. The test therefore asserts the
//! *wiring* (auth + URL + model + WAV upload + fixture decode all succeed
//! and the backend returns a structured `Ok`), not any particular
//! transcript text, matching the Python `groq-integration` smoke's stance.
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

/// Path to the bundled fixture. `CARGO_MANIFEST_DIR` is `src/rust`; the WAV
/// lives under `src/python`.
fn hello_wav_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../python/tests/fixtures/hello.wav")
}

#[test]
fn groq_cloud_stt_transcribes_hello_wav() {
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

    let pcm = decode_wav_16k_mono(&hello_wav_path()).expect("decode bundled hello.wav fixture");
    assert!(!pcm.is_empty(), "hello.wav fixture produced no PCM samples");

    let backend = CloudTranscribeBackend::new(CloudTranscribeConfig {
        base_url: GROQ_BASE_URL.to_owned(),
        api_key,
        model: GROQ_STT_MODEL.to_owned(),
        timeout_ms: 30_000,
        language: None,
        prompt: None,
    });

    // A successful `Ok` proves the full wiring worked: auth accepted, URL +
    // model resolved, WAV upload decoded server-side. We deliberately do NOT
    // assert on `result.text`: the fixture is a synthetic tone, so an empty
    // transcript is a valid Groq response, not a backend failure.
    let result = backend
        .transcribe(&pcm, 16_000)
        .expect("Groq cloud transcription round trip should succeed for the hello.wav clip");
    eprintln!("[groq-cloud-stt] transcript: {:?}", result.text.trim());
}
