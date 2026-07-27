//! Build-time filtering of the `device` settings enum.
//!
//! The `device` field on [`crate::config::settings::AppSettings`] takes the
//! values `auto | cuda | cpu` (mirrors the legacy Python
//! `VOICEPI_DEVICE` knob for faster-whisper). Whether `cuda` actually does
//! anything depends on **which whisper.cpp GPU backend was compiled in**:
//!
//! - `whisper-rs-vulkan` → whisper.cpp's Vulkan backend is linked; the Rust
//!   engine's [`crate::whisper::gpu::should_use_gpu`] resolves any non-`cpu`
//!   device to Vulkan.
//! - `whisper-rs-cuda` (added by a companion release/Cargo change) →
//!   whisper.cpp's CUDA backend is linked; the Rust engine and the Python
//!   faster-whisper path both prefer the NVIDIA GPU.
//! - Neither → asking for `cuda` silently falls back to CPU. That's the bug
//!   this module fixes: on a stock CPU-only binary the Settings UI still
//!   offered `cuda`, the user picked it, and Whisper quietly ran on CPU.
//!
//! The rule is: only show the `cuda` option on binaries that could actually
//! use a GPU. `auto` and `cpu` are always safe (`auto` degrades to CPU
//! gracefully; `cpu` is the safe fallback everywhere).
//!
//! `cfg!(feature = "whisper-rs-cuda")` may reference a feature that is not
//! declared in Cargo.toml yet (the concurrent CUDA-build-flag change adds
//! it). `build.rs` publishes a `rustc-check-cfg` allow-list so the
//! `unexpected_cfgs` lint stays quiet until then; the expression itself
//! evaluates to `false` in the absence of the feature, which is the
//! correct "no CUDA compiled in" behaviour.

/// Config value written to `config.json` / passed as `VOICEPI_DEVICE`.
pub const DEVICE_AUTO: &str = "auto";
/// Config value written to `config.json` / passed as `VOICEPI_DEVICE`.
pub const DEVICE_CUDA: &str = "cuda";
/// Config value written to `config.json` / passed as `VOICEPI_DEVICE`.
pub const DEVICE_CPU: &str = "cpu";

/// Every possible value the `device` field has *ever* accepted, in the order
/// they appear in the UI dropdown when they are all present.
///
/// Callers that need "what should the validator recognise as a legal *config
/// value*, ignoring what this binary can actually do?" want this. Callers
/// that need "what should the UI show / what should the CLI setter accept?"
/// want [`available_device_values`].
pub const ALL_DEVICE_VALUES: &[&str] = &[DEVICE_AUTO, DEVICE_CUDA, DEVICE_CPU];

/// `true` iff any whisper.cpp GPU backend is compiled into this binary.
///
/// Any non-`cpu` device value resolves to "use the GPU" at runtime, and the
/// specific backend (Vulkan today, CUDA when the feature lands, Metal /
/// DirectML in future waves) is selected by whisper.cpp itself. So the
/// dropdown decision "should we offer `cuda`?" is really "is there any GPU
/// backend that can pick up the request?". Keeping this as one predicate
/// means adding a new backend feature is a one-line change here.
#[must_use]
pub const fn any_gpu_backend_compiled() -> bool {
    cfg!(feature = "whisper-rs-vulkan") || cfg!(feature = "whisper-rs-cuda")
}

/// Device values this binary can actually honour, in stable declaration
/// order for a UI dropdown.
///
/// Always includes `auto` and `cpu`; includes `cuda` only when some GPU
/// backend is compiled in. The return type is a heap `Vec` (rather than a
/// `&'static [&'static str]` per-feature-combination table) so the option
/// set stays trivially composable when more backends are added — one
/// `push` per gated variant, no combinatorial `const` slices.
#[must_use]
pub fn available_device_values() -> Vec<&'static str> {
    let mut out = Vec::with_capacity(ALL_DEVICE_VALUES.len());
    out.push(DEVICE_AUTO);
    if any_gpu_backend_compiled() {
        out.push(DEVICE_CUDA);
    }
    out.push(DEVICE_CPU);
    out
}

/// `true` iff `value` is a config value this binary can honour.
///
/// Empty / whitespace values normalise via caller trim; this helper takes
/// the raw string and matches case-insensitively so
/// `whisper-dictate config set device CUDA` is treated the same as
/// `... device cuda`.
#[must_use]
pub fn is_device_supported(value: &str) -> bool {
    let trimmed = value.trim();
    available_device_values()
        .iter()
        .any(|candidate| trimmed.eq_ignore_ascii_case(candidate))
}

/// Human-facing explanation for why `value` is not honoured on this build,
/// or `None` if the value is either supported or not a recognised device.
///
/// Used by:
///
/// - The Settings UI help text below the Device combo, so a user staring
///   at a two-option dropdown (`auto` / `cpu`) learns *why* CUDA isn't
///   listed — the empty spot in the menu doesn't explain itself.
/// - The CLI `config set device cuda` error path on non-GPU builds, so
///   scripting users don't have to grep source to discover the rebuild
///   flag or the CUDA-enabled installer.
///
/// A recognised-but-supported value (e.g. `auto` on any build) returns
/// `None` — there's nothing to explain. An *unrecognised* value also
/// returns `None`; the enum-choice validator already produces a clean
/// "must be one of …" error in that case, and layering a second message
/// on top would double up.
#[must_use]
pub fn missing_device_hint(value: &str) -> Option<&'static str> {
    let trimmed = value.trim();
    if !ALL_DEVICE_VALUES
        .iter()
        .any(|candidate| trimmed.eq_ignore_ascii_case(candidate))
    {
        return None;
    }
    if trimmed.eq_ignore_ascii_case(DEVICE_CUDA) && !any_gpu_backend_compiled() {
        return Some(
            "CUDA acceleration requires a whisper.cpp GPU backend compiled \
             in. Install the GPU build from the releases page, or rebuild \
             with `--features whisper-rs-vulkan` (vendor-agnostic GPU) or \
             `--features whisper-rs-cuda` (NVIDIA-only).",
        );
    }
    None
}

/// Extra help text appended below the Device combo when this build hides
/// one or more choices, so the shrunken menu doesn't look broken. Returns
/// an empty string when every legal device is available (all backends
/// compiled in) so the caller can concatenate unconditionally.
#[must_use]
pub fn missing_device_footnote() -> &'static str {
    if any_gpu_backend_compiled() {
        ""
    } else {
        "\n\nThis build has no GPU backend compiled in, so `cuda` is hidden \
         - it would silently run on CPU anyway. Install the GPU build or \
         rebuild with `--features whisper-rs-vulkan` to enable it."
    }
}
