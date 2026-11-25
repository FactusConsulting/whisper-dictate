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

/// Canonicalise a device value without losing the CUDA selector used by the
/// dynamically loaded Nemotron runtime. Whisper historically treated `cuda`
/// as an alias for its Vulkan build, but Nemotron ships its own CUDA archive
/// and must receive the distinct value unchanged.
#[must_use]
pub fn canonicalize_device_value_for_provider(value: &str, provider: &str) -> String {
    let canonical = value.trim().to_ascii_lowercase();
    if canonical == LEGACY_DEVICE_CUDA && !provider.trim().eq_ignore_ascii_case("nemotron") {
        DEVICE_VULKAN.to_owned()
    } else {
        canonical
    }
}

/// Check a device value using the selected provider's capabilities. Nemotron
/// runtimes are downloaded dynamically, so their Vulkan/CUDA assets remain
/// valid even when the optional whisper.cpp Vulkan feature is absent.
#[must_use]
pub fn is_device_supported_for_provider(value: &str, provider: &str) -> bool {
    let canonical = canonicalize_device_value_for_provider(value, provider);
    if provider.trim().eq_ignore_ascii_case("nemotron") {
        if !matches!(canonical.as_str(), DEVICE_AUTO | DEVICE_VULKAN | DEVICE_CPU)
            && canonical != LEGACY_DEVICE_CUDA
        {
            return false;
        }
        return match canonical.as_str() {
            DEVICE_VULKAN => nemotron_vulkan_runtime_available(),
            LEGACY_DEVICE_CUDA => nemotron_cuda_runtime_available(),
            _ => true,
        };
    }
    is_device_supported(&canonical)
}

/// Whether the pinned Nemotron catalog contains a CUDA runtime for this
/// target. Whisper's Vulkan feature flag is intentionally not consulted:
/// Nemotron ships and loads its own platform archive.
#[must_use]
pub const fn nemotron_cuda_runtime_available() -> bool {
    cfg!(any(
        windows,
        all(target_os = "linux", target_arch = "x86_64")
    ))
}

/// Whether the pinned Nemotron catalog contains a Vulkan runtime for this
/// target. macOS intentionally has only the CPU runtime.
#[must_use]
pub const fn nemotron_vulkan_runtime_available() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

/// Conservative VRAM budget for local Nemotron; zero bytes denotes an
/// official model that has not been downloaded yet.
pub(crate) fn nemotron_required_mib(file_bytes: u64) -> u32 {
    let file_mib = file_bytes.div_ceil(1024 * 1024);
    let required = (file_mib * 3).div_ceil(2) + 1024;
    required.clamp(2048, u64::from(u32::MAX)) as u32
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
