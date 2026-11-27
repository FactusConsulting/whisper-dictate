//! Tests for [`super`] (the `whisper::local` module — `LocalWhisper` struct).
//!
//! WAV-decode tests live in `whisper::wav_tests` (compiled unconditionally
//! without the `whisper-rs-local` feature). Only `LocalWhisper`-specific
//! tests that require the whisper.cpp bindings live here.
//!
//! Extracted from `local.rs` to keep module sizes under the 500-LOC cap;
//! wired in via `#[path = "local_tests.rs"] mod tests;` from `mod.rs`.

use super::*;
use std::env;

#[test]
fn model_selection_reports_best_free_memory_on_adapters_large_enough() {
    let gpus = [
        SpeechGpu {
            index: 0,
            total_mib: 2048,
            free_mib: 2048,
        },
        SpeechGpu {
            index: 1,
            total_mib: 8192,
            free_mib: 512,
        },
        SpeechGpu {
            index: 2,
            total_mib: 4096,
            free_mib: 1024,
        },
    ];
    let error = select_model_adapter(&gpus, 4096).unwrap_err();
    assert_eq!(
        error.downcast_ref::<GpuMemoryShortfall>(),
        Some(&GpuMemoryShortfall {
            required_mib: 4096,
            free_mib: 1024,
        })
    );
    let no_gpu = select_model_adapter(&[], 4096).unwrap_err();
    assert!(no_gpu.to_string().contains("usable Vulkan GPU"));
    let too_small = select_model_adapter(&gpus[..1], 4096).unwrap_err();
    assert!(too_small.downcast_ref::<GpuMemoryShortfall>().is_none());
    assert!(too_small.to_string().contains("requires 4096 MiB"));
}

#[test]
fn inference_state_checks_additional_memory_on_the_loaded_adapter() {
    let model = VulkanModel {
        index: 1,
        required_mib: 4096,
        resident_mib: 3072,
    };
    let mut gpus = [
        SpeechGpu {
            index: 0,
            total_mib: 8192,
            free_mib: 4096,
        },
        SpeechGpu {
            index: 1,
            total_mib: 8192,
            free_mib: 512,
        },
    ];
    let error = model.check_state_capacity_on(&gpus).unwrap_err();
    assert_eq!(
        error.downcast_ref::<GpuMemoryShortfall>(),
        Some(&GpuMemoryShortfall {
            required_mib: 1024,
            free_mib: 512,
        })
    );
    gpus[1].free_mib = 1024;
    assert!(model.check_state_capacity_on(&gpus).is_ok());
}

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
    assert_eq!(normalize_language_hint(None, None), None);
    assert_eq!(normalize_language_hint(None, Some("")), None);
    assert_eq!(normalize_language_hint(None, Some("auto")), None);
    assert_eq!(normalize_language_hint(None, Some("Auto")), None);
    assert_eq!(normalize_language_hint(None, Some("AUTO")), None);
    assert_eq!(normalize_language_hint(None, Some("en")), Some("en"));
}

#[test]
fn edda_decode_overrides_language_and_timestamps_but_explicit_paths_do_not() {
    use crate::whisper::{dispatch::resolve_model, model_manager};

    let english = whisper_rs::get_lang_id("en").expect("English language ID");
    let stock = model_manager::find("large-v3").unwrap();
    for (name, required_mib) in [("edda-v0.2", 2816), ("edda-v0.2-large", 4352)] {
        let entry = model_manager::find(name).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(entry.filename);
        std::fs::write(&path, b"fixture").unwrap();

        assert_eq!(required_gpu_mib(&path, Some(entry)).unwrap(), required_mib);
        assert!(decode_without_timestamps(Some(entry), false));
        for hint in [None, Some("Auto"), Some("en")] {
            let language = normalize_language_hint(Some(entry), hint);
            assert_eq!(language, Some("da"));
            assert_eq!(resolved_language(language, english).as_deref(), Some("da"));
        }

        let path_str = path.to_str().unwrap();
        let unmanaged = resolve_model(Some(path_str), Some(name)).unwrap();
        let unmanaged_by_id = resolve_model(Some(name), Some(name)).unwrap();
        for (model, override_path) in [(&unmanaged, path_str), (&unmanaged_by_id, name)] {
            assert_eq!(model.path.as_path(), Path::new(override_path));
            assert!(model.entry.is_none());
            assert_eq!(normalize_language_hint(model.entry, Some("Auto")), None);
            assert_eq!(normalize_language_hint(model.entry, Some("en")), Some("en"));
            assert!(!decode_without_timestamps(model.entry, false));
            assert!(decode_without_timestamps(model.entry, true));
        }
        let file_estimate = whisper_required_mib(b"fixture".len() as u64);
        assert_eq!(
            required_gpu_mib(&unmanaged.path, unmanaged.entry).unwrap(),
            file_estimate
        );
        assert_eq!(required_gpu_mib(&path, Some(stock)).unwrap(), file_estimate);
    }
    assert_eq!(normalize_language_hint(Some(stock), Some("en")), Some("en"));
    assert!(!decode_without_timestamps(Some(stock), false));
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
    let _guard = crate::test_env_lock::lock_env();
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
    let _guard = crate::test_env_lock::lock_env();
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
    let _guard = crate::test_env_lock::lock_env();
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
    let _guard = crate::test_env_lock::lock_env();
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

#[test]
fn production_load_refuses_cpu_policy_before_native_load() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("model.bin");
    std::fs::write(&path, b"ggml").unwrap();
    let err = unwrap_err(LocalWhisper::with_policy(&path, GpuPolicy::Off, false));
    assert!(
        err.to_string().contains("CPU inference is disabled"),
        "{err}"
    );
}

#[cfg(not(feature = "whisper-rs-vulkan"))]
#[test]
fn production_load_requires_a_vulkan_build() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("model.bin");
    std::fs::write(&path, b"ggml").unwrap();
    for policy in [GpuPolicy::Auto, GpuPolicy::Vulkan] {
        let err = unwrap_err(LocalWhisper::with_policy(&path, policy, false));
        assert!(err.to_string().contains("requires Vulkan support"), "{err}");
    }
}

/// Give the exact Edda Turbo F16 GGML (`ggml-edda-v0.2-f16.bin`) and
/// Danish FLEURS test clip 1839 (`da-fleurs-1839.wav`, 16 kHz mono PCM)
/// via `WHISPER_TEST_EDDA_MODEL_PATH` and `WHISPER_TEST_EDDA_WAV_PATH`.
///
/// From the repository root:
/// `cargo test --manifest-path src/rust/Cargo.toml --lib --features whisper-rs-vulkan edda_turbo_decodes_danish_without_timestamps -- --ignored`
#[cfg(feature = "whisper-rs-vulkan")]
#[test]
#[ignore = "requires Vulkan, the exact Edda Turbo GGML, and the Danish FLEURS WAV"]
fn edda_turbo_decodes_danish_without_timestamps() {
    let model = env::var_os("WHISPER_TEST_EDDA_MODEL_PATH")
        .expect("set WHISPER_TEST_EDDA_MODEL_PATH to the Edda Turbo F16 GGML");
    let wav = env::var_os("WHISPER_TEST_EDDA_WAV_PATH")
        .expect("set WHISPER_TEST_EDDA_WAV_PATH to Danish FLEURS test clip 1839");

    let whisper = LocalWhisper::with_policy(Path::new(&model), GpuPolicy::Vulkan, true)
        .expect("load Edda Turbo on Vulkan");
    let text = whisper
        .transcribe_wav(Path::new(&wav), Some("da"), None)
        .expect("transcribe Danish clip");
    // Timestamp decoding previously produced unrelated foreign-language tokens.
    assert!(
        text.contains("Du skal se skiruten") && text.contains("en lignende vandrerute"),
        "expected the Danish sentence, got {text:?}"
    );
}

#[test]
fn abort_callback_reports_the_caller_flag() {
    let flag = AtomicBool::new(false);
    let user_data = &flag as *const AtomicBool as *mut c_void;
    // SAFETY: `user_data` points at `flag`, which outlives both calls.
    assert!(!unsafe { abort_requested(user_data) });
    flag.store(true, Ordering::Release);
    assert!(unsafe { abort_requested(user_data) });
}

/// End-to-end: a raised abort flag stops whisper.cpp, while a clear one
/// decodes normally. Skipped unless the model + WAV env vars are set.
#[test]
fn abortable_transcribe_stops_when_flag_is_set_when_model_available() {
    let _guard = crate::test_env_lock::lock_env();
    let (Ok(model), Ok(wav)) = (env::var(MODEL_ENV), env::var(WAV_ENV)) else {
        eprintln!("skipping: set {MODEL_ENV} and {WAV_ENV} to run");
        return;
    };
    let whisper = LocalWhisper::new(std::path::Path::new(&model)).expect("load model");
    let samples = decode_wav_16k_mono(std::path::Path::new(&wav)).expect("decode WAV");
    let text = whisper
        .transcribe_samples_abortable(&samples, Some("en"), None, &AtomicBool::new(false))
        .expect("clear flag transcribes");
    assert!(!text.trim().is_empty(), "transcript was empty");
    assert!(whisper
        .transcribe_samples_abortable(&samples, Some("en"), None, &AtomicBool::new(true))
        .is_err());
}
