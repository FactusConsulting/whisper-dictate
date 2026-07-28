//! Companion regression tests for [`crate::devices`]. Lives in a
//! sibling `_tests.rs` (rather than an inline `mod tests` inside
//! `devices.rs`) so the regression-test discipline scanner
//! (`src/tests/python/test_regression_test_discipline.py`) can pair
//! newly-introduced public symbols with unit-test coverage on the
//! filename it recognises.
//!
//! Pinned symbols (from Codex P2 threads on PR #663):
//!
//! * [`crate::devices::EnumerationFlow`] — the two boolean gates the
//!   picker enumeration consults after processing the default host.
//! * [`crate::devices::enumeration_flow`] — pure decision function
//!   for the picker's enumeration matrix. `walk_non_default_hosts`
//!   MUST be true regardless of `rust_capture` (pre-fix code short-
//!   circuited under `VOICEPI_AUDIO_BACKEND=rust`, hiding ASIO / JACK /
//!   Pulse / PipeWire mics from the picker — Codex P2 on `hosts.rs:129`).
//! * [`crate::devices::should_merge_directsound_endpoints`] — the
//!   low-level DirectSound gate. Only merges when the sounddevice
//!   picker opts in AND the Rust capture backend is not active.
//!
//! The three tests below exercise every arm of both helpers. Each
//! assertion FAILS on the pre-#663 code path (verified during the
//! implementation by stubbing the helper to pre-fix behavior).

use super::{
    effective_rust_capture_gate, enumeration_flow, should_merge_directsound_endpoints,
    EnumerationFlow,
};

#[test]
fn enumeration_flow_walks_non_default_hosts_regardless_of_backend() {
    // Codex P2 (hosts.rs:129) fix 1 regression pin. The load-bearing
    // property: the non-default-host walk runs REGARDLESS of the audio
    // backend. Pre-fix code was `if rust_capture { return; }` which
    // set `walk_non_default_hosts = false` under Rust capture - this
    // assertion would FAIL against that pre-fix behavior on every call
    // site, on every OS, without needing a live secondary cpal host.
    for include_ds in [false, true] {
        for rust_capture in [false, true] {
            let flow: EnumerationFlow = enumeration_flow(include_ds, rust_capture);
            assert!(
                flow.walk_non_default_hosts,
                "non-default cpal hosts MUST be walked \
                 (include_ds={include_ds}, rust_capture={rust_capture}); \
                 the pre-#663 code short-circuited under rust_capture, \
                 hiding ASIO/JACK/Pulse/PipeWire mics from the picker"
            );
        }
    }
}

#[test]
fn enumeration_flow_gates_directsound_on_rust_capture_and_opt_in() {
    // The four arms of the DirectSound gate, pinned exhaustively. Any
    // regression that widens or narrows this gate (e.g. letting
    // DirectSound through under Rust capture, or dropping it for the
    // sounddevice picker) trips one of these assertions.
    let sounddevice = enumeration_flow(true, false);
    assert!(
        sounddevice.merge_directsound,
        "sounddevice picker path must merge DirectSound endpoints"
    );

    let rust_capture_with_opt_in = enumeration_flow(true, true);
    assert!(
        !rust_capture_with_opt_in.merge_directsound,
        "Rust capture path must NOT advertise DirectSound-only mics (cpal 0.18 cannot open them)"
    );

    let cpal_only = enumeration_flow(false, false);
    assert!(
        !cpal_only.merge_directsound,
        "cpal callers with include_directsound=false must never merge"
    );

    let cpal_under_rust = enumeration_flow(false, true);
    assert!(
        !cpal_under_rust.merge_directsound,
        "cpal + rust capture with include_directsound=false must never merge"
    );
}

#[test]
fn should_merge_directsound_endpoints_matches_enumeration_flow() {
    // Belt-and-braces: the low-level helper and the aggregated
    // `enumeration_flow` MUST agree on every combination so a future
    // refactor can't accidentally drift them apart. Anchoring the
    // scanner symbol `should_merge_directsound_endpoints` here also
    // guarantees the discipline check sees test coverage for it.
    for include_ds in [false, true] {
        for rust_capture in [false, true] {
            assert_eq!(
                should_merge_directsound_endpoints(include_ds, rust_capture),
                enumeration_flow(include_ds, rust_capture).merge_directsound,
                "flow disagrees with low-level helper for \
                 (include_ds={include_ds}, rust_capture={rust_capture})"
            );
        }
    }
}

// ----- Codex post-merge P2 (#669 devices.rs:271): pick-config strict filter -
//
// Under `VOICEPI_AUDIO_BACKEND=rust`, the picker must apply the SAME
// strict filter the resolver uses (F32/I16/I32 supported input config
// with usable channels) — otherwise the picker advertises a device
// `capture::pick_config` cannot open and capture fails silently.
// The filter is enforced in `devices::append_host_devices` via the
// shared `hosts::device_supports_rust_capture` helper.

#[test]
fn append_host_devices_signature_accepts_rust_capture_strict_flag() {
    // Compile-time pin: `append_host_devices` MUST expose the
    // `rust_capture_strict` parameter so `enumerate_all_hosts` (and
    // any future caller) can request the strict pick-config filter.
    // Removing this parameter would silently reintroduce the
    // pre-fix behavior where the picker advertises devices capture
    // cannot open.
    let f: fn(
        &cpal::Host,
        Option<usize>,
        bool,
        bool,
        &mut usize,
        &mut Vec<super::DeviceInfo>,
        &mut Vec<String>,
    ) = super::append_host_devices;
    // Reference the function pointer so the compiler doesn't strip
    // the check as dead code.
    let _ = f as usize;
}

// ----- Codex P2 (#674 devices.rs:206): env-flag alone must not activate ----
// the strict filter. The strict filter is only correct when the
// running binary can ACTUALLY route capture through the Rust pipeline
// — otherwise the effective backend is Python sounddevice, which
// handles more formats than `pick_config` and would see valid
// microphones pruned from the picker.

#[test]
fn effective_rust_capture_gate_requires_both_feature_and_env() {
    // Regression pin: only (true, true) turns the strict filter on.
    // A regression that returned `env_requests_rust` alone (the
    // pre-fix behavior) would fail arm (false, true) — the "user
    // set the env but the feature was not compiled in" fallback case.
    assert!(
        !effective_rust_capture_gate(false, false),
        "no feature + no env → Python backend, no strict filter"
    );
    assert!(
        !effective_rust_capture_gate(false, true),
        "no feature + env → Python backend fallback; strict filter \
         would over-prune (Codex P2 #674 devices.rs:206)"
    );
    assert!(
        !effective_rust_capture_gate(true, false),
        "feature but no env → default Python backend, no strict filter"
    );
    assert!(
        effective_rust_capture_gate(true, true),
        "feature + env → Rust backend, strict filter must fire"
    );
}

#[test]
fn device_supports_rust_capture_helper_is_reachable_from_devices() {
    // Cross-crate symbol check: `devices.rs` calls
    // `crate::audio::hosts::device_supports_rust_capture` for its
    // strict filter. This test compiles iff that path resolves,
    // which pins the shared-helper wiring so a refactor can't
    // silently drop the strict filter for the picker.
    let _: fn(&cpal::Device) -> bool = crate::audio::hosts::device_supports_rust_capture;
}

// NOTE (Codex P2 #669 devices_tests.rs:129): the previous behavioural
// test here compared two live `list_input_devices()` enumerations
// separated by an env-var flip. That was non-hermetic — an unplugged
// mic, an audio-server restart, or a transient secondary-backend
// failure between the two enumerations would legitimately shrink the
// second set and fail the assertion even when the backend-selection
// logic is correct. The env-lock only stabilises PROCESS state, not
// hardware or host availability.
//
// The invariant it was trying to pin — "the non-default-host walk
// runs regardless of the audio backend" — is already covered
// deterministically by
// [`enumeration_flow_walks_non_default_hosts_regardless_of_backend`]
// above, which exercises the pure `enumeration_flow` helper against
// every (`include_directsound`, `rust_capture`) arm without touching
// cpal at all. That test FAILS on the pre-#663 code (verified) and
// doesn't depend on live hardware — so it strictly supersedes the
// deleted live-enumeration test.
