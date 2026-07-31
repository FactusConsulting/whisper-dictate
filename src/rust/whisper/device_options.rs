//! Build-time filtering for the native transcription device setting.
//!
//! CPU-only builds reject `cuda` instead of accepting it and silently
//! demoting inference to CPU. GPU builds expose it when their whisper.cpp
//! backend is compiled in.

pub const DEVICE_AUTO: &str = "auto";
pub const DEVICE_CUDA: &str = "cuda";
pub const DEVICE_CPU: &str = "cpu";

/// Every historically recognised value, used for bounded hint generation.
pub const ALL_DEVICE_VALUES: &[&str] = &[DEVICE_AUTO, DEVICE_CUDA, DEVICE_CPU];

#[must_use]
pub const fn any_gpu_backend_compiled() -> bool {
    cfg!(feature = "whisper-rs-vulkan")
}

/// Values this native binary can actually honour.
#[must_use]
pub fn available_device_values() -> Vec<&'static str> {
    let mut values = Vec::with_capacity(ALL_DEVICE_VALUES.len());
    values.push(DEVICE_AUTO);
    if any_gpu_backend_compiled() {
        values.push(DEVICE_CUDA);
    }
    values.push(DEVICE_CPU);
    values
}

#[must_use]
pub fn is_device_supported(value: &str) -> bool {
    let canonical = canonicalize_device_value(value);
    available_device_values()
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&canonical))
}

#[must_use]
pub fn canonicalize_device_value(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// Explain why a recognised device is unavailable in this build.
#[must_use]
pub fn missing_device_hint(value: &str) -> Option<&'static str> {
    let canonical = canonicalize_device_value(value);
    if !ALL_DEVICE_VALUES.contains(&canonical.as_str()) {
        return None;
    }
    if canonical == DEVICE_CUDA && !any_gpu_backend_compiled() {
        return Some(
            "CUDA is unavailable in this native build because no whisper.cpp \
             GPU backend is compiled in. Install the GPU build from the \
             releases page, or rebuild with `--features whisper-rs-vulkan` \
             (vendor-agnostic GPU).",
        );
    }
    None
}

#[must_use]
pub fn missing_device_footnote() -> &'static str {
    if any_gpu_backend_compiled() {
        ""
    } else {
        "\n\nThis build has no native GPU backend, so `cuda` is unavailable. \
         Install the GPU build or rebuild with `--features whisper-rs-vulkan`."
    }
}
