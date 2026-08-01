//! Companion tests for the `post_set_engine_hint` warning wiring in
//! [`crate::config::handle_command`]. Split out of the inline
//! `#[cfg(test)] mod tests` block in `mod.rs` so the regression-test
//! discipline scanner
//! (`src/tests/python/test_regression_test_discipline.py`) — which
//! resolves `mod.rs` → `mod_tests.rs` — sees a matching companion
//! file next to the production module.
//!
//! The end-to-end coverage that runs the compiled `whisper-dictate`
//! binary and asserts stdout + stderr on `config set device vulkan`
//! lives in `src/rust/tests/config_cli_device.rs` — this file pins
//! only the pure wrapper.
//!
//! #667  .

#![cfg(test)]

use super::post_set_engine_hint;
use crate::whisper::device_options::any_gpu_backend_compiled;

// -----------------------------------------------------------------
// #655 r3663634825 — post-set engine hint. `config set
// device vulkan` on a CPU-only Rust build is rejected. Keep the native
// rebuild guidance available to the CLI without requiring users to read
// `docs/CONFIGURATION.md`.
// -----------------------------------------------------------------

#[test]
fn post_set_engine_hint_none_for_non_device_keys() {
    // Only the `device` key has an engine-split hint today. Other
    // keys must never trigger a spurious warning — a `model` set
    // to `"large-v3-turbo"` is universally accepted.
    assert!(post_set_engine_hint("model", "large-v3-turbo").is_none());
    assert!(post_set_engine_hint("audio_device", "Yeti").is_none());
    assert!(post_set_engine_hint("stt_backend", "openai").is_none());
}

#[test]
fn post_set_engine_hint_none_for_universally_supported_device_values() {
    // `auto` and `cpu` work on every build regardless of compiled
    // GPU backend; must not trip the warning.
    assert!(post_set_engine_hint("device", "auto").is_none());
    assert!(post_set_engine_hint("device", "cpu").is_none());
    assert!(post_set_engine_hint("device", "  AUTO  ").is_none());
}

#[test]
fn post_set_engine_hint_names_vulkan_on_cpu_only_rust_build() {
    // On a CPU-only Rust build (no `whisper-rs-vulkan` feature),
    // `missing_device_hint` returns
    // Some(...) for `vulkan` (and legacy `cuda`); the wrapper must surface it.
    // On a build WITH a GPU backend the hint is None (nothing to
    // explain), so this only asserts wrapping in the CPU-only
    // configuration this test crate is built with.
    if any_gpu_backend_compiled() {
        // No hint expected on GPU builds — nothing to test.
        assert!(post_set_engine_hint("device", "vulkan").is_none());
        return;
    }
    let warning =
        post_set_engine_hint("device", "vulkan").expect("vulkan on CPU-only Rust build must warn");
    assert!(
        warning.starts_with("warning: "),
        "warning must have a leading `warning: ` prefix so a scripting user \
         can grep for it, got: {warning:?}",
    );
    assert!(
        warning.contains("unavailable") && warning.contains("Vulkan"),
        "warning must name the unavailable native backend, got: {warning:?}",
    );
    assert!(!warning.to_ascii_lowercase().contains("python"));
}

#[test]
fn post_set_engine_hint_canonicalises_before_checking() {
    // The CLI setter canonicalises the value before persisting, but
    // the caller (handle_command) passes the RAW argv string here.
    // Uppercase / whitespace input must produce the same warning as
    // the canonical form so users don't get inconsistent messaging
    // depending on how they typed the value.
    if any_gpu_backend_compiled() {
        return; // no hint on GPU builds — nothing to test
    }
    assert!(post_set_engine_hint("device", "cuda").is_some());
    assert!(post_set_engine_hint("device", "vulkan").is_some());
    assert!(post_set_engine_hint("device", "CUDA").is_some());
    assert!(post_set_engine_hint("device", "  cuda\t").is_some());
}
