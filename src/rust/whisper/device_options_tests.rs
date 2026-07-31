use super::device_options::{
    any_gpu_backend_compiled, available_device_values, canonicalize_device_value,
    is_device_supported, missing_device_footnote, missing_device_hint, ALL_DEVICE_VALUES,
    DEVICE_AUTO, DEVICE_CPU, DEVICE_CUDA,
};

#[test]
fn auto_and_cpu_are_always_offered_in_stable_order() {
    let values = available_device_values();
    assert_eq!(values.first(), Some(&DEVICE_AUTO));
    assert_eq!(values.last(), Some(&DEVICE_CPU));
}

#[test]
fn cuda_availability_matches_the_native_build() {
    assert_eq!(
        available_device_values().contains(&DEVICE_CUDA),
        any_gpu_backend_compiled()
    );
    assert_eq!(is_device_supported("  CUDA  "), any_gpu_backend_compiled());
}

#[test]
fn dropdown_is_a_unique_subset_of_canonical_values() {
    let values = available_device_values();
    assert!(values.iter().all(|value| ALL_DEVICE_VALUES.contains(value)));
    let mut unique = values.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), values.len());
}

#[test]
fn supported_values_are_case_and_whitespace_insensitive() {
    for value in ["auto", " AUTO ", "\tauto\n", "cpu", " CPU "] {
        assert!(is_device_supported(value), "{value:?}");
    }
    for value in ["rocm", "opencl", "", "   "] {
        assert!(!is_device_supported(value), "{value:?}");
    }
}

#[test]
#[cfg(not(feature = "whisper-rs-vulkan"))]
fn cpu_only_build_rejects_cuda_with_native_rebuild_guidance() {
    assert!(!is_device_supported("cuda"));
    let hint = missing_device_hint("cuda").expect("cuda rebuild hint");
    assert!(hint.contains("unavailable"));
    assert!(hint.contains("whisper-rs-vulkan"));
    assert!(!hint.to_ascii_lowercase().contains("python"));
    let footnote = missing_device_footnote();
    assert!(footnote.contains("cuda"));
    assert!(footnote.contains("unavailable"));
    assert!(!footnote.to_ascii_lowercase().contains("python"));
}

#[test]
#[cfg(feature = "whisper-rs-vulkan")]
fn gpu_build_accepts_cuda_without_a_missing_device_hint() {
    assert!(is_device_supported("cuda"));
    assert!(missing_device_hint("cuda").is_none());
    assert_eq!(missing_device_footnote(), "");
}

#[test]
fn missing_hint_ignores_supported_and_unknown_values() {
    assert!(missing_device_hint("auto").is_none());
    assert!(missing_device_hint("cpu").is_none());
    assert!(missing_device_hint("rocm").is_none());
}

#[test]
fn canonicalization_is_bounded_and_idempotent() {
    for (raw, expected) in [
        ("  CUDA  ", "cuda"),
        ("Auto", "auto"),
        ("\tCPU\n", "cpu"),
        ("  GPU  ", "gpu"),
    ] {
        let once = canonicalize_device_value(raw);
        assert_eq!(once, expected);
        assert_eq!(canonicalize_device_value(&once), once);
    }
}
