//! Build-time filtering for the native transcription device setting.
//!
//! CPU-only builds reject `vulkan` instead of accepting it and silently
//! demoting inference to CPU. GPU builds expose the actual backend name.

pub const DEVICE_AUTO: &str = "auto";
pub const DEVICE_VULKAN: &str = "vulkan";
pub const LEGACY_DEVICE_CUDA: &str = "cuda";
pub const DEVICE_CPU: &str = "cpu";

/// Every historically recognised value, used for bounded hint generation.
pub const ALL_DEVICE_VALUES: &[&str] =
    &[DEVICE_AUTO, DEVICE_VULKAN, LEGACY_DEVICE_CUDA, DEVICE_CPU];

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
        values.push(DEVICE_VULKAN);
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
    let canonical = value.trim().to_ascii_lowercase();
    if canonical == LEGACY_DEVICE_CUDA {
        DEVICE_VULKAN.to_owned()
    } else {
        canonical
    }
}

/// Explain why a recognised device is unavailable in this build.
#[must_use]
pub fn missing_device_hint(value: &str) -> Option<&'static str> {
    let canonical = canonicalize_device_value(value);
    if !ALL_DEVICE_VALUES.contains(&canonical.as_str()) {
        return None;
    }
    if canonical == DEVICE_VULKAN && !any_gpu_backend_compiled() {
        return Some(
            "Vulkan is unavailable in this native build because the whisper.cpp \
             Vulkan backend is not compiled in. Install the GPU build from the \
             releases page, or rebuild with `--features whisper-rs-vulkan`.",
        );
    }
    None
}

#[must_use]
pub fn missing_device_footnote() -> &'static str {
    if any_gpu_backend_compiled() {
        ""
    } else {
        "\n\nThis build has no native GPU backend, so `vulkan` is unavailable. \
         Install the GPU build or rebuild with `--features whisper-rs-vulkan`."
    }
}
