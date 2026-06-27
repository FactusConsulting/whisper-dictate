//! GPU policy parsing for the local Whisper inference path.
//!
//! Split out from `local.rs` so the parse rules can be exercised in pure
//! unit tests without touching whisper.cpp or requiring a GPU backend to
//! be compiled in. The module is **always** compiled (just like
//! [`super::idle::env`]) so the env-var contract is consistent across
//! builds — only the resolution of the policy to an actual
//! `WhisperContextParameters.use_gpu` boolean depends on the
//! `whisper-rs-vulkan` feature.
//!
//! Wave 7-C of roadmap issue #348: first GPU backend for the Rust
//! `whisper-rs-local` transcription path. Vulkan was chosen over CUDA /
//! DirectML / Metal because it is the only backend that covers both
//! Windows AND Linux from a single feature flag, vendor-agnostically.
//! Further backends (DirectML, Metal) will land as additional features
//! that add their own variant to [`GpuPolicy`].

use anyhow::{anyhow, Result};

/// Environment variable that selects the GPU policy for the local Whisper
/// inference path.
///
/// Value semantics (parsed by [`parse_gpu_policy_from_env`]):
/// - **Unset, empty, `auto`, `default`** → [`GpuPolicy::Auto`]: use GPU iff
///   the binary was built with a GPU backend feature, CPU otherwise.
/// - **`off`, `cpu`, `0`, `false`, `no`** → [`GpuPolicy::Off`]: never ask
///   whisper.cpp for GPU even if a backend is compiled in.
/// - **`vulkan`** → [`GpuPolicy::Vulkan`]: prefer Vulkan; falls back to CPU
///   silently if the build doesn't include `whisper-rs-vulkan`.
///
/// All matching is case-insensitive — env vars get spelt differently across
/// shells, UI dropdowns, and shell-history paste. Anything else is a hard
/// error rather than a silent fallback, matching the philosophy of
/// [`super::idle::env::IDLE_UNLOAD_ENV`]: a user that has expressed an
/// intent should learn about the typo, not get the default they didn't
/// ask for.
pub const GPU_ENV: &str = "VOICEPI_WHISPER_GPU";

/// Which GPU policy to apply when constructing a
/// [`whisper_rs::WhisperContextParameters`].
///
/// The selection is decoupled from the actual `whisper-rs` feature flags:
/// `Auto` and `Vulkan` both ask for GPU at runtime, but whether the
/// underlying whisper.cpp library actually has a GPU backend compiled in is
/// a build concern. [`should_use_gpu`] resolves the final boolean by
/// combining the policy with `cfg!(feature = "whisper-rs-vulkan")`.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub enum GpuPolicy {
    /// CPU only — never ask whisper.cpp for GPU even if a backend is built in.
    Off,
    /// Use GPU if the binary was built with a GPU backend feature; CPU otherwise.
    #[default]
    Auto,
    /// Use Vulkan specifically. Falls through to CPU if the build doesn't
    /// include `whisper-rs-vulkan`; the runtime UX is "best effort with a
    /// clear log line at startup" rather than refusing to run.
    Vulkan,
}

/// Read [`GPU_ENV`] and parse it into a [`GpuPolicy`].
///
/// See the constant's docs for the value grammar. The three error modes
/// from [`std::env::var`] are handled distinctly:
///
/// - `NotPresent` → `Ok(GpuPolicy::Auto)` (no override, use the build's default).
/// - `NotUnicode` → `Err` — an explicitly set value we can't decode is a
///   configuration bug worth surfacing loudly.
/// - `Ok(raw)` → delegate to [`parse_gpu_policy_str`].
pub fn parse_gpu_policy_from_env() -> Result<GpuPolicy> {
    match std::env::var(GPU_ENV) {
        Err(std::env::VarError::NotPresent) => Ok(GpuPolicy::default()),
        Err(std::env::VarError::NotUnicode(raw)) => Err(anyhow!(
            "{GPU_ENV}={raw:?} is not valid UTF-8; \
             use 'auto', 'off', or 'vulkan'"
        )),
        Ok(raw) => parse_gpu_policy_str(&raw),
    }
}

/// Pure helper for the env-var parser, split out for unit testing without
/// having to mutate the process environment.
pub(super) fn parse_gpu_policy_str(raw: &str) -> Result<GpuPolicy> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(GpuPolicy::default());
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "off" | "cpu" | "0" | "false" | "no" => Ok(GpuPolicy::Off),
        "auto" | "default" => Ok(GpuPolicy::Auto),
        "vulkan" => Ok(GpuPolicy::Vulkan),
        _ => Err(anyhow!(
            "{GPU_ENV}={raw:?} is not a recognised GPU policy; \
             use 'auto', 'off', or 'vulkan' (synonyms: 'cpu'/'0'/'false'/'no' for off, \
             'default' for auto)"
        )),
    }
}

/// Resolve the final `WhisperContextParameters.use_gpu` boolean from a
/// [`GpuPolicy`], taking the compiled-in feature flags into account.
///
/// - `Off` → `false` unconditionally.
/// - `Auto` → `true` iff a GPU backend feature is compiled in.
/// - `Vulkan` → `true` iff `whisper-rs-vulkan` is compiled in. We don't
///   error here because the runtime UX is "best effort with a clear log
///   line"; refusing to run when the build is missing the feature would
///   convert a graceful fallback into a hard failure for users who set
///   `VOICEPI_WHISPER_GPU=vulkan` on a stock binary.
pub fn should_use_gpu(policy: GpuPolicy) -> bool {
    match policy {
        GpuPolicy::Off => false,
        // `Auto` and `Vulkan` both reduce to the same boolean today because
        // Vulkan is the only GPU backend we ship. When DirectML / Metal land
        // each will get its own cfg-OR clause here and a matching variant
        // arm so `Auto` keeps meaning "any GPU we have" while the named
        // policies pick a specific one.
        GpuPolicy::Auto | GpuPolicy::Vulkan => cfg!(feature = "whisper-rs-vulkan"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::ENV_LOCK;

    // -- pure parser ------------------------------------------------------

    #[test]
    fn parse_empty_means_auto() {
        assert_eq!(parse_gpu_policy_str("").unwrap(), GpuPolicy::Auto);
        assert_eq!(parse_gpu_policy_str("   ").unwrap(), GpuPolicy::Auto);
        assert_eq!(parse_gpu_policy_str("\t\n").unwrap(), GpuPolicy::Auto);
    }

    #[test]
    fn parse_off_synonyms_case_insensitive() {
        for s in [
            "off", "OFF", "Off", "cpu", "CPU", "Cpu", "0", "false", "False", "FALSE", "no", "NO",
        ] {
            assert_eq!(
                parse_gpu_policy_str(s).unwrap(),
                GpuPolicy::Off,
                "expected Off for {s:?}"
            );
        }
    }

    #[test]
    fn parse_auto_synonyms_case_insensitive() {
        for s in ["auto", "Auto", "AUTO", "default", "Default", "DEFAULT"] {
            assert_eq!(
                parse_gpu_policy_str(s).unwrap(),
                GpuPolicy::Auto,
                "expected Auto for {s:?}"
            );
        }
    }

    #[test]
    fn parse_vulkan_case_insensitive() {
        for s in ["vulkan", "Vulkan", "VULKAN", "vUlKaN"] {
            assert_eq!(
                parse_gpu_policy_str(s).unwrap(),
                GpuPolicy::Vulkan,
                "expected Vulkan for {s:?}"
            );
        }
    }

    #[test]
    fn parse_trims_whitespace_around_value() {
        assert_eq!(
            parse_gpu_policy_str("  vulkan  ").unwrap(),
            GpuPolicy::Vulkan
        );
        assert_eq!(parse_gpu_policy_str("\toff\n").unwrap(), GpuPolicy::Off);
    }

    #[test]
    fn parse_unknown_errors_with_helpful_hint() {
        let err = parse_gpu_policy_str("cuda").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(GPU_ENV), "missing env name: {msg}");
        assert!(msg.contains("auto"), "missing valid values list: {msg}");
        assert!(msg.contains("vulkan"), "missing valid values list: {msg}");
    }

    #[test]
    fn parse_unknown_preserves_raw_value_in_error() {
        let err = parse_gpu_policy_str("rocm").unwrap_err();
        assert!(
            err.to_string().contains("rocm"),
            "raw value should appear in error: {err}"
        );
    }

    #[test]
    fn parse_arbitrary_garbage_is_an_error_not_silent_default() {
        // Catches the regression where a typo silently meant "Auto".
        for s in ["yes", "true", "on", "gpu", "metal", "directml", "1"] {
            assert!(
                parse_gpu_policy_str(s).is_err(),
                "{s:?} should be rejected — not a recognised policy"
            );
        }
    }

    // -- default ----------------------------------------------------------

    #[test]
    fn default_policy_is_auto() {
        assert_eq!(GpuPolicy::default(), GpuPolicy::Auto);
    }

    // -- feature-aware resolution -----------------------------------------

    #[test]
    fn should_use_gpu_off_is_always_false() {
        // Off must NEVER ask for GPU, no matter what features are compiled in.
        assert!(!should_use_gpu(GpuPolicy::Off));
    }

    #[test]
    #[cfg(feature = "whisper-rs-vulkan")]
    fn should_use_gpu_true_when_vulkan_feature_compiled_in() {
        assert!(should_use_gpu(GpuPolicy::Auto));
        assert!(should_use_gpu(GpuPolicy::Vulkan));
    }

    #[test]
    #[cfg(not(feature = "whisper-rs-vulkan"))]
    fn should_use_gpu_false_when_no_gpu_backend_compiled_in() {
        // On a CPU-only build, Auto and Vulkan both reduce to false — this
        // is the silent-fallback behaviour we promise so a user that sets
        // VOICEPI_WHISPER_GPU=vulkan on a stock binary still gets a working
        // transcribe path.
        assert!(!should_use_gpu(GpuPolicy::Auto));
        assert!(!should_use_gpu(GpuPolicy::Vulkan));
    }

    // -- env-var integration ----------------------------------------------

    #[test]
    fn parse_env_unset_means_auto() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var(GPU_ENV).ok();
        std::env::remove_var(GPU_ENV);

        let got = parse_gpu_policy_from_env().unwrap();
        assert_eq!(got, GpuPolicy::Auto);

        if let Some(v) = saved {
            std::env::set_var(GPU_ENV, v);
        }
    }

    #[test]
    fn parse_env_reads_value() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var(GPU_ENV).ok();
        std::env::set_var(GPU_ENV, "vulkan");

        let got = parse_gpu_policy_from_env().unwrap();
        assert_eq!(got, GpuPolicy::Vulkan);

        match saved {
            Some(v) => std::env::set_var(GPU_ENV, v),
            None => std::env::remove_var(GPU_ENV),
        }
    }

    #[test]
    fn parse_env_rejects_unknown_with_env_name_in_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var(GPU_ENV).ok();
        std::env::set_var(GPU_ENV, "tensorrt");

        let err = parse_gpu_policy_from_env().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(GPU_ENV), "{msg}");
        assert!(msg.contains("tensorrt"), "{msg}");

        match saved {
            Some(v) => std::env::set_var(GPU_ENV, v),
            None => std::env::remove_var(GPU_ENV),
        }
    }
}
