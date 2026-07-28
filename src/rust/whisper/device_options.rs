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
//!
//! The `device` setting is shared with the Python faster-whisper fallback
//! engine (`VOICEPI_DICTATE_ENGINE=python`), which honours `cuda` via
//! CTranslate2 regardless of which whisper.cpp GPU backend is compiled
//! into the Rust engine. So `cuda` is a legal *config value* on every
//! build; what `any_gpu_backend_compiled()` gates is whether the *Rust*
//! engine can honour it. When the Rust engine can't but the Python
//! fallback can, we still offer `cuda` and explain the fallback in the
//! Settings-UI hint so scripting users and `runtime/install_plan.rs`
//! (which drives `requirements/gpu.txt` off a saved `device = "cuda"`)
//! both keep working.

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

/// `true` iff the Python faster-whisper fallback engine (selected via
/// `VOICEPI_DICTATE_ENGINE=python`) is shipped in this binary and can
/// honour `device = "cuda"` via CTranslate2.
///
/// There is no Cargo feature gating the Python engine today — it is a
/// separate worker process launched by [`crate::runtime::supervisor`] and
/// ships in every build. If a `python-engine` feature is ever introduced
/// this returns `cfg!(feature = "python-engine")` instead; until then it
/// is unconditionally `true`, which is what keeps `cuda` a legal config
/// value on CPU-only Rust builds so `runtime/install_plan.rs`'s
/// `wants_cuda_runtime()` can still trigger `requirements/gpu.txt`.
#[must_use]
pub const fn python_engine_can_use_cuda() -> bool {
    true
}

/// Device values this binary can actually honour, in stable declaration
/// order for a UI dropdown.
///
/// Always includes `auto` and `cpu`; includes `cuda` when EITHER a
/// whisper.cpp GPU backend is compiled into the Rust engine OR the
/// Python fallback can honour it. The return type is a heap `Vec`
/// (rather than a `&'static [&'static str]` per-feature-combination
/// table) so the option set stays trivially composable when more
/// backends are added — one `push` per gated variant, no combinatorial
/// `const` slices.
#[must_use]
pub fn available_device_values() -> Vec<&'static str> {
    let mut out = Vec::with_capacity(ALL_DEVICE_VALUES.len());
    out.push(DEVICE_AUTO);
    if any_gpu_backend_compiled() || python_engine_can_use_cuda() {
        out.push(DEVICE_CUDA);
    }
    out.push(DEVICE_CPU);
    out
}

/// `true` iff `value` is a config value this binary can honour.
///
/// Values are canonicalised via [`canonicalize_device_value`] before
/// membership check, so `whisper-dictate config set device "  CUDA  "`
/// is treated the same as `... device cuda`.
#[must_use]
pub fn is_device_supported(value: &str) -> bool {
    let canonical = canonicalize_device_value(value);
    available_device_values()
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&canonical))
}

/// Canonicalise a user-supplied `device` value so callers can persist
/// exactly what the Python fallback's `_resolve_device` (case-sensitive,
/// no-trim) and the Rust validator both accept: trim surrounding
/// whitespace and lower-case ASCII.
///
/// `"  CUDA  "` → `"cuda"`, `"Auto\t"` → `"auto"`. Non-ASCII characters
/// are left alone (device names are pure ASCII today; a hand-edited
/// garbage value with Unicode falls through to the validator's
/// "must be one of …" error, which is the intended UX for typos).
#[must_use]
pub fn canonicalize_device_value(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// Human-facing explanation for why `value` is not honoured by the
/// **Rust** engine on this build, or `None` if the value is either
/// natively supported by the Rust engine or not a recognised device.
///
/// Used by:
///
/// - The Settings UI help text below the Device combo, so a user picking
///   `cuda` on a CPU-only Rust build learns that only the Python fallback
///   engine (`VOICEPI_DICTATE_ENGINE=python`) will honour it.
/// - The CLI `config set device cuda` help text on non-GPU Rust builds,
///   so scripting users don't have to grep source to discover the rebuild
///   flag or the CUDA-enabled installer.
///
/// A value the Rust engine natively supports (e.g. `auto` on any build,
/// `cuda` on a Vulkan/CUDA-enabled build) returns `None` — there's
/// nothing to explain. An *unrecognised* value also returns `None`; the
/// enum-choice validator already produces a clean "must be one of …"
/// error in that case, and layering a second message on top would double
/// up. `cuda` on a CPU-only Rust build returns a hint even though the
/// value is accepted, because the user needs to know the Rust engine
/// will silently fall back to CPU unless they switch engines or rebuild.
#[must_use]
pub fn missing_device_hint(value: &str) -> Option<&'static str> {
    let canonical = canonicalize_device_value(value);
    if !ALL_DEVICE_VALUES
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&canonical))
    {
        return None;
    }
    if canonical == DEVICE_CUDA && !any_gpu_backend_compiled() {
        return Some(
            "CUDA is honoured only by the Python faster-whisper fallback \
             engine on this build (set VOICEPI_DICTATE_ENGINE=python). \
             The native Rust engine has no whisper.cpp GPU backend \
             compiled in and will silently fall back to CPU. To use CUDA \
             with the Rust engine, install the GPU build from the \
             releases page, or rebuild with `--features whisper-rs-vulkan` \
             (vendor-agnostic GPU) or `--features whisper-rs-cuda` \
             (NVIDIA-only).",
        );
    }
    None
}

/// Extra help text appended below the Device combo when this build has
/// no whisper.cpp GPU backend compiled into the Rust engine, so users
/// understand the engine-dependent behaviour of the `cuda` choice.
/// Returns an empty string when a GPU backend is compiled in (the
/// choice is unambiguous everywhere) so the caller can concatenate
/// unconditionally.
#[must_use]
pub fn missing_device_footnote() -> &'static str {
    if any_gpu_backend_compiled() {
        ""
    } else {
        "\n\nThis build has no GPU backend compiled into the native Rust \
         engine, so `cuda` will silently fall back to CPU there. The \
         Python faster-whisper fallback engine \
         (VOICEPI_DICTATE_ENGINE=python) still honours `cuda` via \
         CTranslate2. Install the GPU build or rebuild with \
         `--features whisper-rs-vulkan` to enable CUDA in the Rust engine."
    }
}
