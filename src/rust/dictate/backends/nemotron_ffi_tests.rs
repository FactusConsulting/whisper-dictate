use std::path::Path;
#[cfg(any(windows, target_os = "linux"))]
use std::path::PathBuf;

use super::{
    library_is_loadable, platform_loader_fallback, speech_phrase_values, NativeRecognizer,
};

#[test]
fn dictionary_terms_are_individual_native_speech_context_phrases() {
    let terms = vec!["Codex".to_owned(), "Cloudflare".to_owned()];

    assert_eq!(
        speech_phrase_values(Some("Vocabulary: Codex, Cloudflare"), &terms),
        vec!["Codex", "Cloudflare"]
    );
}

#[test]
fn custom_prompt_is_used_when_dictionary_has_no_terms() {
    assert_eq!(
        speech_phrase_values(Some("Project Aurora"), &[]),
        vec!["Project Aurora"]
    );
}

#[test]
fn windows_does_not_use_a_bare_dll_loader_fallback() {
    if cfg!(windows) {
        assert!(platform_loader_fallback().is_none());
    } else {
        assert!(platform_loader_fallback().is_some());
    }
}

#[test]
fn dictionary_phrases_replace_the_composed_prompt() {
    let terms = vec!["Kubernetes".to_owned(), "Cloudflare".to_owned()];
    assert_eq!(
        speech_phrase_values(Some("Kubernetes, Cloudflare"), &terms),
        vec!["Kubernetes", "Cloudflare"]
    );
}

#[test]
fn raw_dictionary_phrases_are_used_without_a_prompt() {
    let terms = vec![
        " Kubernetes ".to_owned(),
        "".to_owned(),
        "Cloudflare".to_owned(),
    ];
    assert_eq!(
        speech_phrase_values(None, &terms),
        vec![" Kubernetes ", "Cloudflare"]
    );
}

#[test]
fn loader_probe_rejects_a_missing_soname() {
    assert!(!library_is_loadable(Path::new(
        "whisper-dictate-library-that-does-not-exist.so",
    )));
}

/// Build a tiny hermetic implementation of the public NeMo-Speech.cpp C ABI.
///
/// The production runtime and model cannot be bundled in CI, but a real
/// dynamic library still exercises symbol resolution, request construction,
/// result decoding, and destruction without reaching a network service.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn build_fixture_library(directory: &Path) -> PathBuf {
    let source = directory.join("nemotron_ffi_fixture.rs");
    let library = directory.join(if cfg!(windows) {
        "nemotron_ffi_fixture.dll"
    } else {
        "libnemotron_ffi_fixture.so"
    });
    std::fs::write(
        &source,
        r#"
use std::ffi::{c_char, c_void};
use std::ptr;
#[repr(C)] struct BackendConfig { size: usize, gpu: i32 }
#[repr(C)] struct ModelConfig { size: usize, path: *const c_char, name: *const c_char }
#[repr(C)] pub struct RecognizerConfig { size: usize, backend: *const BackendConfig, model: *const ModelConfig, streaming: *const c_void, decoder: *const c_void, vad: *const c_void, endpointing: *const c_void, postproc: *const c_void, diar: *const c_void, batching: *const c_void }
#[repr(C)] struct SpeechContext { size: usize, phrases: *const *const c_char, phrase_count: usize, boost: f32 }
#[repr(C)] pub struct RecognitionOptions { size: usize, request_id: *const c_char, language_code: *const c_char, interim_results: bool, enable_word_time_offsets: bool, enable_automatic_punctuation: bool, verbatim_transcripts: bool, profanity_filter: bool, stop_history_eou_ms: i32, speech_contexts: *const SpeechContext, speech_context_count: usize, max_alternatives: i32, enable_speaker_diarization: bool, max_speaker_count: i32 }
#[no_mangle] pub extern "C" fn nemo_speech_asr_create(_: *const RecognizerConfig, out: *mut *mut c_void) -> i32 { unsafe { *out = Box::into_raw(Box::new(1_u8)) as *mut c_void; } 0 }
#[no_mangle] pub extern "C" fn nemo_speech_asr_destroy(raw: *mut c_void) { if !raw.is_null() { unsafe { drop(Box::from_raw(raw as *mut u8)); } } }
#[no_mangle] pub extern "C" fn nemo_speech_asr_recognition_options_default() -> RecognitionOptions { RecognitionOptions { size: 0, request_id: ptr::null(), language_code: ptr::null(), interim_results: false, enable_word_time_offsets: false, enable_automatic_punctuation: false, verbatim_transcripts: false, profanity_filter: false, stop_history_eou_ms: 0, speech_contexts: ptr::null(), speech_context_count: 0, max_alternatives: 0, enable_speaker_diarization: false, max_speaker_count: 0 } }
#[no_mangle] pub extern "C" fn nemo_speech_asr_recognize_f32(_: *mut c_void, _: *const RecognitionOptions, _: *const f32, samples: usize, _: i32, out: *mut *mut c_void) -> i32 { if samples == 0 { return 7; } unsafe { *out = Box::into_raw(Box::new(1_u8)) as *mut c_void; } 0 }
#[no_mangle] pub extern "C" fn nemo_speech_asr_result_transcript(_: *const c_void, _: usize) -> *const c_char { b"fixture transcript\0".as_ptr() as *const c_char }
#[no_mangle] pub extern "C" fn nemo_speech_asr_result_language_count(_: *const c_void, _: usize) -> usize { 1 }
#[no_mangle] pub extern "C" fn nemo_speech_asr_result_language_code(_: *const c_void, _: usize, _: usize) -> *const c_char { b"en-US\0".as_ptr() as *const c_char }
#[no_mangle] pub extern "C" fn nemo_speech_asr_result_destroy(raw: *mut c_void) { if !raw.is_null() { unsafe { drop(Box::from_raw(raw as *mut u8)); } } }
#[no_mangle] pub extern "C" fn nemo_speech_asr_last_error() -> *const c_char { b"fixture native error\0".as_ptr() as *const c_char }
"#,
    )
    .expect("write Nemotron FFI fixture source");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let status = std::process::Command::new(rustc)
        .args(["--edition=2021", "--crate-type=cdylib"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .status()
        .expect("start rustc for Nemotron FFI fixture");
    assert!(status.success(), "build Nemotron FFI fixture: {status}");
    library
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn fixture_library_exercises_the_dynamic_abi_end_to_end() {
    let directory = tempfile::tempdir().expect("temporary native ABI fixture directory");
    let library = build_fixture_library(directory.path());
    let model = directory.path().join("fixture.gguf");
    std::fs::write(&model, b"fixture model").expect("write fixture model");

    let recognizer =
        NativeRecognizer::new(&library, &model, -1).expect("load fixture native recognizer");
    let result = recognizer
        .recognize(
            &[0.2, -0.2],
            16_000,
            "en-US",
            Some("Vocabulary: Codex"),
            &["Codex".to_owned()],
        )
        .expect("fixture recognition");
    assert_eq!(result.text, "fixture transcript");
    assert_eq!(result.language.as_deref(), Some("en-US"));

    let error = recognizer
        .recognize(&[], 16_000, "en-US", None, &[])
        .expect_err("fixture rejects empty audio");
    assert!(error.to_string().contains("fixture native error"));
}
