//! Regression tests for [`super::device_options`].
//!
//! These lock in the original #648 "silently accepts `cuda` and silently
//! runs on CPU" bug — the shipping Windows rc.9 binary was built without
//! any GPU backend feature and still offered `cuda` in the Settings UI
//! dropdown; picking it made faster-whisper log a warning and quietly
//! demote to CPU — AND the Codex follow-up on #648 which flagged that
//! the fix must not conflate whisper.cpp compile-time features with the
//! shared `device` setting (the Python faster-whisper fallback engine
//! honours `cuda` via CTranslate2 on every build, and
//! `runtime/install_plan.rs::wants_cuda_runtime` reads the saved setting
//! to install `requirements/gpu.txt`).
//!
//! The current contract: `cuda` is a legal config value on every build;
//! the Settings-UI hint / footnote explains when only the Python fallback
//! engine can honour it and the Rust engine will silently fall back to
//! CPU. Each assertion is written to work on **both** feature
//! configurations (GPU-backend on OR off), gating with
//! `cfg!(feature = ...)` where the message text differs.

use super::device_options::{
    any_gpu_backend_compiled, available_device_values, canonicalize_device_value,
    is_device_supported, missing_device_footnote, missing_device_hint, python_engine_can_use_cuda,
    ALL_DEVICE_VALUES, DEVICE_AUTO, DEVICE_CPU, DEVICE_CUDA,
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
fn cuda_is_offered_when_any_engine_can_honour_it() {
    // Codex P1 from #648: `cuda` is a legal config value on every build
    // as long as EITHER a whisper.cpp GPU backend is compiled into the
    // Rust engine OR the Python faster-whisper fallback engine can honour
    // it via CTranslate2. Hiding it on a CPU-only Rust build breaks the
    // Python fallback path and `runtime/install_plan.rs::wants_cuda_runtime`,
    // which drives `requirements/gpu.txt` off a saved `device = "cuda"`.
    let values = available_device_values();
    let cuda_offered = values.contains(&DEVICE_CUDA);
    let expected = any_gpu_backend_compiled() || python_engine_can_use_cuda();
    assert_eq!(
        cuda_offered,
        expected,
        "cuda offered?={cuda_offered} but any_gpu_backend_compiled()={} \
         || python_engine_can_use_cuda()={} — dropdown must agree with \
         what SOME engine on this build can honour",
        any_gpu_backend_compiled(),
        python_engine_can_use_cuda(),
    );
}

#[test]
fn cuda_is_always_offered_today() {
    // Lock the current invariant that the Python fallback ships in every
    // build. If a `python-engine` feature is ever added, update
    // `python_engine_can_use_cuda` to be feature-gated and this test to
    // mirror the new gating.
    assert!(python_engine_can_use_cuda());
    assert!(available_device_values().contains(&DEVICE_CUDA));
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
fn is_device_supported_accepts_cuda_on_every_build() {
    // Codex P1 (#648 device_options thread): `cuda` is a legal config
    // value regardless of the compiled Rust GPU features because the
    // Python fallback engine honours it via CTranslate2. Refusing it on
    // CPU-only Rust builds would break `runtime/install_plan.rs`'s
    // `wants_cuda_runtime` and the Python engine path both.
    assert!(is_device_supported("cuda"));
    assert!(is_device_supported("CUDA"));
    assert!(is_device_supported("  cuda\n"));
}

// -- missing_device_hint --------------------------------------------------

#[test]
#[cfg(not(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda")))]
fn missing_device_hint_explains_engine_split_on_cpu_only_rust_builds() {
    // Codex P1 (#648): `cuda` is accepted on every build now, but on a
    // CPU-only Rust build the hint must still tell the user that only the
    // Python fallback engine (VOICEPI_DICTATE_ENGINE=python) will honour
    // the choice, and point at the rebuild flag / GPU installer for
    // in-Rust CUDA.
    let hint = missing_device_hint("cuda").expect("cuda hint missing");
    assert!(
        hint.to_lowercase().contains("python"),
        "hint should name the Python fallback engine, got: {hint}",
    );
    assert!(
        hint.contains("whisper-rs") || hint.contains("Vulkan") || hint.contains("vulkan"),
        "hint should name the rebuild flag / GPU install path, got: {hint}",
    );
    // Casing / whitespace must not matter — the CLI accepts uppercase.
    assert!(missing_device_hint("CUDA").is_some());
    assert!(missing_device_hint("  cuda  ").is_some());
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
fn footnote_explains_engine_split_on_cpu_only_rust_builds() {
    let note = missing_device_footnote();
    assert!(
        !note.is_empty(),
        "CPU-only Rust build must render a why-cuda-behaves-oddly footnote",
    );
    let lower = note.to_lowercase();
    assert!(lower.contains("cuda"), "footnote: {note}");
    assert!(
        lower.contains("python"),
        "footnote must name the Python fallback engine so users know \
         cuda is not silently broken here, got: {note}",
    );
}

#[test]
#[cfg(any(feature = "whisper-rs-vulkan", feature = "whisper-rs-cuda"))]
fn footnote_is_empty_when_all_backends_available() {
    // When nothing is hidden there is no "why" to explain; a stray
    // footnote in that case would confuse users on a full-featured build.
    assert_eq!(missing_device_footnote(), "");
}

// -- canonicalize_device_value (Codex #648 P2) ----------------------------

#[test]
fn canonicalize_strips_whitespace_and_lowercases_ascii() {
    // Codex P2: `"  CUDA  "` on disk breaks Python
    // `vp_cli._resolve_device` (case-insensitive but does NOT trim); the
    // CLI setter must persist the canonical form both engines accept.
    assert_eq!(canonicalize_device_value("  CUDA  "), "cuda");
    assert_eq!(canonicalize_device_value("Auto"), "auto");
    assert_eq!(canonicalize_device_value("\tCPU\n"), "cpu");
}

#[test]
fn canonicalize_is_idempotent_on_canonical_values() {
    // Round-trip guarantee: applying the transform twice equals applying
    // it once, so callers can canonicalise defensively without corrupting
    // an already-canonical value.
    for value in [DEVICE_AUTO, DEVICE_CPU, DEVICE_CUDA] {
        let once = canonicalize_device_value(value);
        let twice = canonicalize_device_value(&once);
        assert_eq!(once, value);
        assert_eq!(once, twice);
    }
}

#[test]
fn canonicalize_leaves_unknown_values_recognisable_for_the_validator() {
    // The transform must not swallow typos into a valid form — it only
    // trims + lowercases. `"GPU"` (invalid) canonicalises to `"gpu"`,
    // which still fails `is_device_supported` and lets the caller surface
    // a clean error rather than silently rewriting to `"auto"`.
    let canonical = canonicalize_device_value("  GPU  ");
    assert_eq!(canonical, "gpu");
    assert!(!is_device_supported(&canonical));
}
