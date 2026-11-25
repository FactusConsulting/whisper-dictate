use super::*;
use std::path::PathBuf;

/// Missing-file error surfaces as `Errored`, not `Panicked`. The distinction
/// matters to diagnostic callers: `Errored` is expected on a fresh install
/// (no model downloaded yet); `Panicked` is the "something is very wrong"
/// signal.
#[test]
fn load_catch_unwind_missing_file_returns_errored() {
    let bogus = PathBuf::from("/definitely/not/a/real/path/model.bin");
    // Can't use `expect_err` — `LocalWhisper: !Debug` (see the module-level
    // note; whisper-rs's `WhisperContext` doesn't implement Debug and
    // forwarding an FFI pointer would be meaningless anyway). Match the Err
    // variant manually.
    let failure = match LocalWhisper::load_catch_unwind(&bogus) {
        Err(f) => f,
        Ok(_) => panic!("load of {} must fail", bogus.display()),
    };
    assert_eq!(failure.kind(), "errored");
    assert!(
        failure.message().contains("not found") || failure.message().contains("open"),
        "unexpected message: {}",
        failure.message()
    );
}

#[test]
fn diagnostic_model_refuses_cpu_inference_when_model_available() {
    let Some(model) = std::env::var_os("WHISPER_TEST_MODEL_PATH") else {
        return;
    };
    let whisper = LocalWhisper::load_catch_unwind(Path::new(&model))
        .expect("diagnostic loads model weights on CPU");
    let err = whisper
        .transcribe_samples(&[0.0], Some("en"), None)
        .expect_err("diagnostic must not create a CPU inference state");
    assert!(err.to_string().contains("load-only"), "{err}");
}

#[test]
fn load_failure_display_is_kind_colon_message() {
    let e = LoadFailure::errored(anyhow::anyhow!("out of memory"));
    assert_eq!(format!("{e}"), "errored: out of memory");
    let p = LoadFailure::Panicked("boom".to_owned());
    assert_eq!(format!("{p}"), "panicked: boom");
}

#[test]
fn load_failure_clone_preserves_variant_and_message() {
    let e = LoadFailure::errored(anyhow::anyhow!("out of memory"));
    let cloned = e.clone();
    assert_eq!(cloned.kind(), "errored");
    assert!(cloned.message().contains("out of memory"));
}

/// Panic payloads that aren't `String` / `&str` (e.g. `panic!(42)`) still
/// produce a non-empty message so the log line is useful.
#[test]
fn panic_payload_to_string_handles_non_string_payload() {
    let payload: Box<dyn std::any::Any + Send> = Box::new(42_i32);
    let msg = panic_payload_to_string(&payload);
    assert!(msg.contains("OOM") || msg.contains("non-string"));
}
