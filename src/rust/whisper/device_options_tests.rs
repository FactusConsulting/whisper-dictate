//! Regression tests for [`super::device_options`].
//!
//! These lock in the "silently accepts `cuda` and silently runs on CPU"
//! bug — the shipping Windows rc.9 binary was built without any GPU
//! backend feature and still offered `cuda` in the Settings UI dropdown;
//! picking it made faster-whisper log a warning and quietly demote to
//! CPU. The regression this suite guards is "dropdown option set exactly
//! matches what the compiled build can honour, and the CLI setter
//! rejects any value that the dropdown hides".
//!
//! Each assertion is written to work on **both** feature configurations
//! (GPU-backend on OR off), gating with `cfg!(feature = ...)` so the
//! same test file runs green in every CI leg without duplicated fixtures.

use super::device_options::{
    any_gpu_backend_compiled, available_device_values, is_device_supported,
    missing_device_footnote, missing_device_hint, ALL_DEVICE_VALUES, DEVICE_AUTO, DEVICE_CPU,
    DEVICE_CUDA,
};

// -- available_device_values ---------------------------------------------

#[test]
fn auto_and_cpu_are_always_offered() {
    // The two backend-independent choices must be present on every build,
    // otherwise the user has no way to force CPU-only inference on a
    // GPU-enabled binary — a regression that would silently ignore
    // `VOICEPI_DEVICE=cpu`.
    let values = available_device_values();
    assert!(values.contains(&DEVICE_AUTO), "missing auto: {values:?}");
    assert!(values.contains(&DEVICE_CPU), "missing cpu: {values:?}");
}

#[test]
fn cuda_present_iff_any_gpu_backend_compiled() {
    // Headline contract: what the dropdown shows == what the binary can do.
    let values = available_device_values();
    let cuda_offered = values.contains(&DEVICE_CUDA);
    assert_eq!(
        cuda_offered,
        any_gpu_backend_compiled(),
        "cuda listed?={cuda_offered} but any_gpu_backend_compiled()={} — \
         these must always agree, else the UI offers a silently-broken option",
        any_gpu_backend_compiled(),
    );
}

#[test]
fn dropdown_order_places_auto_first_and_cpu_last() {
    // Stable ordering matches the pre-fix dropdown so users' muscle memory
    // (`auto` at top, `cpu` at bottom) survives the feature gating.
    let values = available_device_values();
    assert_eq!(values.first(), Some(&DEVICE_AUTO), "auto must be first");
    assert_eq!(values.last(), Some(&DEVICE_CPU), "cpu must be last");
}

#[test]
fn dropdown_is_a_subset_of_the_all_devices_table() {
    // The validator loads `ALL_DEVICE_VALUES` for the "legal but silently
    // hidden" set; the UI must never invent a value that isn't in that
    // canonical list, else a saved config could become unloadable.
    let values = available_device_values();
    for candidate in &values {
        assert!(
            ALL_DEVICE_VALUES.contains(candidate),
            "dropdown value {candidate:?} is not in ALL_DEVICE_VALUES = {ALL_DEVICE_VALUES:?}",
        );
    }
}

#[test]
fn dropdown_has_no_duplicates() {
    let values = available_device_values();
    let mut sorted = values.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        values.len(),
        "duplicate device value in {values:?}",
    );
}

// -- is_device_supported --------------------------------------------------

#[test]
fn is_device_supported_accepts_case_and_whitespace_variants_of_auto() {
    // Users paste values from docs / shell history where casing and stray
    // whitespace vary. `AUTO`, `  auto`, `Auto\n` all mean the same thing.
    for value in ["auto", "AUTO", "Auto", "  auto  ", "\tauto\n"] {
        assert!(
            is_device_supported(value),
            "should accept normalised auto: {value:?}",
        );
    }
}

#[test]
fn is_device_supported_matches_available_values_membership() {
    // Every offered value passes; nothing in the offered set gets rejected.
    for value in available_device_values() {
        assert!(
            is_device_supported(value),
            "available value {value:?} rejected by is_device_supported",
        );
    }
}

#[test]
fn is_device_supported_rejects_unknown_values() {
    for value in ["rocm", "opencl", "directml", "tpu", "", "   "] {
        assert!(
            !is_device_supported(value),
            "unknown value {value:?} should be rejected",
        );
    }
}

#[test]
#[cfg(not(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda")))]
fn is_device_supported_rejects_cuda_on_cpu_only_builds() {
    // The headline regression: the shipping rc.9 CPU-only build silently
    // accepted `cuda` and demoted to CPU. Now it must refuse the value
    // at validation time so the CLI setter surfaces the error.
    assert!(!is_device_supported("cuda"));
    assert!(!is_device_supported("CUDA"));
}

#[test]
#[cfg(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda"))]
fn is_device_supported_accepts_cuda_on_gpu_builds() {
    assert!(is_device_supported("cuda"));
    assert!(is_device_supported("CUDA"));
}

// -- missing_device_hint --------------------------------------------------

#[test]
#[cfg(not(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda")))]
fn missing_device_hint_explains_cuda_on_cpu_only_builds() {
    let hint = missing_device_hint("cuda").expect("cuda hint missing");
    assert!(
        hint.contains("CUDA") && (hint.contains("vulkan") || hint.contains("whisper-rs")),
        "hint should name the feature to enable, got: {hint}",
    );
    // Casing must not matter — the CLI accepts uppercase.
    assert!(missing_device_hint("CUDA").is_some());
}

#[test]
#[cfg(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda"))]
fn missing_device_hint_returns_none_for_cuda_on_gpu_builds() {
    // On a build that supports it, cuda is not missing — nothing to explain.
    assert!(missing_device_hint("cuda").is_none());
}

#[test]
fn missing_device_hint_returns_none_for_supported_values() {
    // `auto` and `cpu` are always available; asking about them makes no sense
    // and should not produce a phantom "why is X missing?" line.
    assert!(missing_device_hint("auto").is_none());
    assert!(missing_device_hint("cpu").is_none());
}

#[test]
fn missing_device_hint_returns_none_for_unknown_values() {
    // The enum-choice validator handles "not a recognised value" errors; a
    // second layer of "why is rocm missing?" would just be noise.
    assert!(missing_device_hint("rocm").is_none());
    assert!(missing_device_hint("").is_none());
}

// -- missing_device_footnote ----------------------------------------------

#[test]
#[cfg(not(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda")))]
fn footnote_explains_missing_cuda_on_cpu_only_builds() {
    let note = missing_device_footnote();
    assert!(
        !note.is_empty(),
        "CPU-only build must render a why-cuda-is-hidden footnote",
    );
    assert!(note.to_lowercase().contains("cuda"), "footnote: {note}");
}

#[test]
#[cfg(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda"))]
fn footnote_is_empty_when_all_backends_available() {
    // When nothing is hidden there is no "why" to explain; a stray
    // footnote in that case would confuse users on a full-featured build.
    assert_eq!(missing_device_footnote(), "");
}
