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
    enumeration_flow, list_input_devices, should_merge_directsound_endpoints, EnumerationFlow,
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

#[test]
fn enumerate_all_hosts_walks_non_default_hosts_under_rust_capture() {
    // Direct behavioural regression for the `hosts.rs:129` fix. Pre-
    // fix code executed `if rust_capture { return out; }` right after
    // the default host, so under `VOICEPI_AUDIO_BACKEND=rust` the
    // result was strictly the default-host subset. Post-fix the env
    // var no longer gates the non-default-host walk.
    //
    // On a headless CI box with only one cpal host, both branches
    // return the same set — but the property we can test
    // deterministically is that the Rust-capture set is never a strict
    // subset of the default-host subset. Any secondary host that
    // exists MUST also appear under rust_capture. Env-mutating tests
    // share process state, so hold the crate-wide lock.
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("VOICEPI_AUDIO_BACKEND");

    // 1) Baseline: env unset (Python-shaped enumeration, no DS merge).
    std::env::remove_var("VOICEPI_AUDIO_BACKEND");
    let baseline = list_input_devices();
    let baseline_names: std::collections::BTreeSet<String> =
        baseline.iter().map(|d| d.name.clone()).collect();

    // 2) Under Rust capture — MUST include every baseline entry (the
    //    fix removed the early return that pruned non-default hosts).
    std::env::set_var("VOICEPI_AUDIO_BACKEND", "rust");
    let under_rust = list_input_devices();
    let under_rust_names: std::collections::BTreeSet<String> =
        under_rust.iter().map(|d| d.name.clone()).collect();

    // Restore env before any assertion so a failed assert doesn't leak
    // the mutation into other tests.
    match prev {
        Some(v) => std::env::set_var("VOICEPI_AUDIO_BACKEND", v),
        None => std::env::remove_var("VOICEPI_AUDIO_BACKEND"),
    }

    for name in &baseline_names {
        assert!(
            under_rust_names.contains(name),
            "device {name:?} enumerated without rust_capture is missing \
             under rust_capture; the pre-fix early return would drop \
             non-default-host mics here",
        );
    }
}
