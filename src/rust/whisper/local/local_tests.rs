//! Tests for [`super`] (the `whisper::local` module — `LocalWhisper` struct).
//!
//! WAV-decode tests live in `whisper::wav_tests` (compiled unconditionally
//! without the `whisper-rs-local` feature). Only `LocalWhisper`-specific
//! tests that require the whisper.cpp bindings live here.
//!
//! Extracted from `local.rs` to keep module sizes under the 500-LOC cap;
//! wired in via `#[path = "local_tests.rs"] mod tests;` from `mod.rs`.

use super::*;
use crate::test_env_lock::ENV_LOCK;
use std::env;

/// Env vars pointing at a downloaded whisper.cpp model file and a
/// matching "hello world" WAV. CI does not set these, so the heavy
/// end-to-end test is skipped there; a developer running
///
/// ```sh
/// WHISPER_TEST_MODEL_PATH=/path/to/ggml-tiny.en.bin \
/// WHISPER_TEST_WAV_PATH=/path/to/hello.wav \
/// cargo test --features whisper-rs-local
/// ```
///
/// exercises the full path. We don't ship the model (30+ MB) or a real
/// "hello world" recording in the repo — see the README "Local
/// Whisper" section for where to grab one.
const MODEL_ENV: &str = "WHISPER_TEST_MODEL_PATH";
const WAV_ENV: &str = "WHISPER_TEST_WAV_PATH";

/// Small helper: `Result::unwrap_err` requires the `Ok` value (here a
/// `LocalWhisper` wrapping a non-Debug `WhisperContext`) to implement
/// `Debug`; this avoids the bound without forcing `Debug` onto our
/// public type.
fn unwrap_err<T>(r: anyhow::Result<T>) -> anyhow::Error {
    match r {
        Ok(_) => panic!("expected an error, got Ok"),
        Err(e) => e,
    }
}

#[test]
fn auto_language_hint_uses_whisper_transcription_mode() {
    // A null language hint asks whisper.cpp to detect the language and keep
    // decoding. The separate `detect_language` parameter is detection-only;
    // the production path must not turn Auto into a language probe that
    // returns an empty segment list.
    assert_eq!(normalize_language_hint(None), None);
    assert_eq!(normalize_language_hint(Some("auto")), None);
    assert_eq!(normalize_language_hint(Some("en")), Some("en"));
}

#[test]
fn auto_detected_language_is_reported_for_postprocessing() {
    let english = whisper_rs::get_lang_id("en").expect("English language ID");
    assert_eq!(resolved_language(None, english).as_deref(), Some("en"));
    assert_eq!(
        resolved_language(Some("da"), english).as_deref(),
        Some("da")
    );
}

#[test]
fn new_rejects_missing_model() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let err = unwrap_err(LocalWhisper::new(std::path::Path::new(
        "/definitely/not/a/real/model.bin",
    )));
    assert!(
        err.to_string().contains("not found"),
        "unexpected error: {err}"
    );
}

/// Loading a path whose file name contains non-ASCII characters must
/// not panic or silently mangle the path. On Windows valid UTF-8 paths
/// must succeed (we only error on the very rare unpaired-surrogate
/// case); on Unix `tempdir` + non-ASCII file names round-trip fine.
/// Either way we expect a clean "not found"-style error when the file
/// doesn't exist, not a panic, and a "model file not found" message
/// since we point at a non-existent path under the non-ASCII directory.
#[test]
fn new_handles_non_ascii_path_cleanly() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("æøå-日本語-model.bin");
    // File does not exist — we just want to confirm the error message
    // surfaces the path without panicking on Display / to_str.
    let err = unwrap_err(LocalWhisper::new(&path));
    let msg = err.to_string();
    assert!(
        msg.contains("not found"),
        "expected missing-file error, got: {msg}"
    );
}

/// GGUF files start with the magic bytes `GGUF`; whisper.cpp can't read
/// them yet and would otherwise produce an opaque FFI error.
#[test]
fn new_rejects_gguf_model_with_clear_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fake.gguf");
    let mut f = std::fs::File::create(&path).unwrap();
    // Magic + a bit of filler so the file exists with the right header.
    f.write_all(b"GGUF\x00\x00\x00\x00more bytes").unwrap();
    f.sync_all().unwrap();
    drop(f);

    let err = unwrap_err(LocalWhisper::new(&path));
    let msg = err.to_string();
    assert!(
        msg.contains("GGUF") && msg.contains("GGML"),
        "expected GGUF/GGML guidance, got: {msg}"
    );
}

/// End-to-end: load a real model, transcribe a known speech WAV in Auto mode,
/// and assert both text and the detected language are returned. Skipped unless
/// the developer
/// opts in via the two env vars below because both the model (30+ MB)
/// and a representative recording are too large/non-portable for the
/// repo.
#[test]
fn auto_language_transcribes_known_speech_when_model_available() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (Ok(model), Ok(wav)) = (env::var(MODEL_ENV), env::var(WAV_ENV)) else {
        eprintln!(
            "skipping: set {MODEL_ENV} (GGML whisper model) and {WAV_ENV} \
             (16 kHz mono speech WAV) to run"
        );
        return;
    };

    let whisper = LocalWhisper::new(std::path::Path::new(&model)).expect("load model");
    // Auto must detect a language and continue into transcription. Before
    // the regression fix, the detection-only flag made this return an empty
    // segment list even though whisper.cpp logged a detected language.
    let samples = decode_wav_16k_mono(std::path::Path::new(&wav)).expect("decode WAV");
    let (text, language) = whisper
        .transcribe_samples_with_language(&samples, Some("auto"), None)
        .expect("transcribe");
    assert!(!text.trim().is_empty(), "auto transcript was empty");
    assert!(language.is_some(), "auto language was not reported");
}
