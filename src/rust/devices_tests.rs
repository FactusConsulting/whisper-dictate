//! Tests for device enumeration and capture gating.
//! They cover non-default hosts, DirectSound merging, strict capture
//! filtering, and picker visibility.

use super::{
    effective_rust_capture_gate, enumeration_flow, in_process_capture_features_present,
    should_merge_directsound_endpoints, should_publish_device, EnumerationFlow,
};

#[test]
fn enumeration_flow_walks_non_default_hosts_regardless_of_backend() {
    // The load-bearing
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
                 a short-circuit under rust_capture, \
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

// Strict capture mode uses the same openability filter as the capture path.
//
// Under `VOICEPI_AUDIO_BACKEND=rust`, the picker must apply the SAME
// strict filter the resolver uses (F32/I16/I32 supported input config
// with usable channels) — otherwise the picker advertises a device
// `capture::pick_config` cannot open and capture fails silently.
// The filter is enforced in `devices::append_host_devices` via the
// shared `hosts::device_supports_rust_capture` helper.

// Test the pure publishing decision without requiring live audio hardware.
//
// `should_publish_device` is the pure decision point
// `append_host_devices` consults for EVERY enumerated device. Testing
// it exhaustively catches regressions such as ignoring
// `rust_capture_strict`, inverting the predicate, or hard-coding one
// return value — WITHOUT depending on live audio hardware (headless CI
// runners have no mics, so a live-enumeration test would vacuously
// pass there).

#[test]
fn should_publish_device_rejects_zero_channel_devices_in_every_mode() {
    // Channels == 0 means no backend can open it — reject regardless
    // of strictness or the openability flag.
    for strict in [false, true] {
        for openable in [false, true] {
            assert!(
                !should_publish_device(0, strict, openable),
                "zero-channel device must never be published \
                 (strict={strict}, openable={openable})"
            );
        }
    }
}

#[test]
fn should_publish_device_publishes_any_channel_bearing_device_when_not_strict() {
    // Non-strict = the Python sounddevice backend is effective. It
    // handles more formats than `pick_config`, so a channel-bearing
    // device is published even when the Rust-capture predicate said
    // no. A regression that ALWAYS applied the strict filter would
    // fail the `openable=false` arm here — that's the over-pruning
    // over-pruning case.
    assert!(
        should_publish_device(1, false, false),
        "non-strict mode must publish a U16-only / default-config-only \
         mic that the Python backend can still open"
    );
    assert!(should_publish_device(2, false, true));
    assert!(should_publish_device(8, false, false));
}

#[test]
fn should_publish_device_requires_openability_when_strict() {
    // Strict = Rust capture is effective. Only devices `pick_config`
    // can open may be published, otherwise the user picks a mic that
    // fails to capture. A regression that IGNORED `rust_capture_strict`
    // (the pre-fix behavior) would fail the `openable=false` arm.
    assert!(
        !should_publish_device(1, true, false),
        "strict mode must NOT publish a device pick_config cannot open"
    );
    assert!(
        !should_publish_device(8, true, false),
        "channel count does not rescue an unopenable device in strict mode"
    );
    assert!(
        should_publish_device(1, true, true),
        "strict mode publishes devices pick_config CAN open"
    );
    assert!(should_publish_device(2, true, true));
}

#[test]
fn should_publish_device_matrix_is_exactly_as_documented() {
    // Full truth table in one place, mirroring the doc-comment on
    // `should_publish_device`. An inverted predicate or a hard-coded
    // return value trips at least one row.
    let cases: &[(u16, bool, bool, bool)] = &[
        // (channels, strict, openable, expected_publish)
        (0, false, false, false),
        (0, false, true, false),
        (0, true, false, false),
        (0, true, true, false),
        (1, false, false, true),
        (1, false, true, true),
        (1, true, false, false),
        (1, true, true, true),
    ];
    for &(channels, strict, openable, expected) in cases {
        assert_eq!(
            should_publish_device(channels, strict, openable),
            expected,
            "matrix mismatch for (channels={channels}, strict={strict}, \
             openable={openable})"
        );
    }
}

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

// The environment flag alone must not activate strict capture filtering.
// the strict filter. The strict filter is only correct when the
// running binary can ACTUALLY route capture through the Rust pipeline
// — otherwise the effective backend is Python sounddevice, which
// handles more formats than `pick_config` and would see valid
// microphones pruned from the picker.

#[test]
fn effective_rust_capture_gate_requires_the_feature_for_every_route() {
    // Without `audio-in-rust` there is no cpal capture path at all, so
    // the effective backend is Python sounddevice regardless of which
    // route asked for Rust. The strict filter must stay OFF or it
    // over-prunes U16-only / default-config-only mics the Python
    // backend can open.
    for env_rust in [false, true] {
        for in_process in [false, true] {
            assert!(
                !effective_rust_capture_gate(false, env_rust, in_process),
                "feature absent must disable the strict filter \
                 (env_rust={env_rust}, in_process={in_process})"
            );
        }
    }
}

#[test]
fn effective_rust_capture_gate_fires_for_the_legacy_worker_audio_optin() {
    // `VOICEPI_AUDIO_BACKEND=rust` route, in-process engine inactive.
    assert!(
        effective_rust_capture_gate(true, true, false),
        "feature + VOICEPI_AUDIO_BACKEND=rust → strict filter must fire"
    );
}

#[test]
fn effective_rust_capture_gate_fires_for_the_default_in_process_engine() {
    // The shipping default. `VOICEPI_DICTATE_ENGINE` unset resolves to the
    // in-process Rust engine, whose pump opens AudioPipeline (cpal)
    // directly without ever consulting `VOICEPI_AUDIO_BACKEND`. The
    // pre-fix 2-arg gate returned false here, leaving the strict
    // filter OFF and the DirectSound merge ON while cpal was the
    // active capture route.
    assert!(
        effective_rust_capture_gate(true, false, true),
        "feature + in-process Rust engine (env unset) → strict filter \
         MUST fire; this is the default shipping configuration"
    );
}

// Base the gate on the installed capture path. `in_process::try_install` is
// feature-gated, so the strict filter must stay off when that path is absent.

#[test]
fn in_process_capture_features_require_rust_hotkeys() {
    // With the capture installation incomplete, the strict filter must not
    // claim that the unavailable in-process path is active.
    assert!(
        !in_process_capture_features_present(
            /*audio_in_rust=*/ true, /*whisper_rs_local=*/ true,
            /*rust_injection=*/ true, /*rust_hotkeys=*/ false,
        ),
        "without rust-hotkeys, in_process::try_install is the \
         FeaturesMissing stub and the strict filter must stay off"
    );
}

#[test]
fn in_process_capture_features_require_every_link_in_the_chain() {
    // Each feature is individually load-bearing: dropping any ONE
    // breaks the in-process cpal capture route, so the predicate must
    // be false for every single-omission combination.
    let all_present = [true, true, true, true];
    for omit in 0..4 {
        let mut flags = all_present;
        flags[omit] = false;
        assert!(
            !in_process_capture_features_present(flags[0], flags[1], flags[2], flags[3]),
            "omitting feature #{omit} must disable the in-process \
             capture route (flags={flags:?})"
        );
    }
    assert!(
        in_process_capture_features_present(true, true, true, true),
        "all four features present → the in-process cpal route exists"
    );
}

#[test]
fn effective_rust_capture_gate_stays_off_when_no_route_is_active() {
    // Feature compiled in, but the operator opted out of the
    // in-process engine (`VOICEPI_DICTATE_ENGINE=python`) and did not
    // set the legacy worker-audio flag → Python sounddevice is
    // effective, so no strict filtering.
    assert!(
        !effective_rust_capture_gate(true, false, false),
        "feature present but NO Rust-capture route active → Python \
         backend, no strict filter"
    );
}

// Exercise the real picker path.
//
// The earlier Windows verification targeted `hosts::snapshot_all_hosts`,
// which has NO production picker callers (diagnostic-only listing).
// The Settings picker actually runs
// `list_input_devices_for_ui_json_line` → `enumerate_all_hosts` →
// `append_host_devices`, so regressions in the `rust_capture_strict`
// threading and DirectSound gating stayed invisible on Windows.
//
// The two tests below drive that REAL path on the `rust-features
// (windows-2025, audio, --features audio-in-rust, test)` CI job.
//
// Hermeticity note: both are SINGLE-enumeration invariant checks, NOT
// comparisons of two live enumerations across an env flip (the
// non-hermetic pattern. Whatever hardware exists at the moment of the
// call, the returned set must satisfy the invariant; a device that
// disappears mid-test is skipped rather than failing the assertion.

#[cfg(feature = "audio-in-rust")]
#[test]
fn picker_under_rust_capture_only_lists_capture_openable_devices() {
    // On the real
    // picker path. Under `VOICEPI_AUDIO_BACKEND=rust` with the
    // `audio-in-rust` feature compiled in, EVERY device
    // `list_input_devices()` publishes MUST satisfy
    // `hosts::device_supports_rust_capture` — otherwise the picker
    // advertises a mic `capture::pick_config` cannot open and capture
    // fails immediately after the user selects it.
    //
    // A regression that dropped the `rust_capture_strict` threading
    // (or inverted the predicate) would surface a U16-only /
    // default-config-only device here and trip the assertion.
    //
    // Deliberately NOT `#[cfg(windows)]`: the invariant holds on every
    // platform, so this runs on BOTH the `rust-features
    // (windows-2025, audio, ...)` and `(ubuntu-latest, audio, ...)` CI
    // legs — Windows coverage plus Linux/ALSA
    // coverage for free. Gated on the feature because the strict
    // filter only activates when `audio-in-rust` is compiled in (see
    // `effective_rust_capture_gate`).
    use cpal::traits::HostTrait;

    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("VOICEPI_AUDIO_BACKEND");
    std::env::set_var("VOICEPI_AUDIO_BACKEND", "rust");

    let published = super::list_input_devices();

    // Restore env BEFORE asserting so a failure can't leak the
    // mutation into other tests.
    match prev {
        Some(v) => std::env::set_var("VOICEPI_AUDIO_BACKEND", v),
        None => std::env::remove_var("VOICEPI_AUDIO_BACKEND"),
    }

    if published.is_empty() {
        // Headless CI runner with no mics — nothing to verify.
        return;
    }

    // Re-look-up each published name across every cpal host and
    // verify SOME device carrying that name passes the strict
    // predicate.
    //
    // Aggregate with `any(...)`
    // across ALL same-named devices rather than stopping at the first
    // match. When two cpal devices share a display name and only the
    // LATER one is capture-openable, the strict picker correctly
    // publishes the later device — a first-match-wins check would
    // record the earlier `false` and fail spuriously. That
    // same-named default/secondary-host case is precisely what the
    // production change exists to support.
    //
    // A name we can no longer find at all (hot-unplug between the two
    // calls) is SKIPPED, keeping the test hermetic.
    for info in &published {
        let mut seen_any = false;
        let mut any_openable = false;
        for host_id in cpal::available_hosts() {
            let Ok(host) = cpal::host_from_id(host_id) else {
                continue;
            };
            let Ok(devices) = host.input_devices() else {
                continue;
            };
            for device in devices {
                if device.to_string() == info.name {
                    seen_any = true;
                    if crate::audio::hosts::device_supports_rust_capture(&device) {
                        any_openable = true;
                    }
                }
            }
        }
        if seen_any {
            assert!(
                any_openable,
                "picker published {:?} under an active Rust-capture route, \
                 but NO device carrying that name can be opened by \
                 capture::pick_config. The rust_capture_strict filter in \
                 append_host_devices is not being applied.",
                info.name
            );
        }
        // else: device vanished between enumerations — skip.
    }
}

// The same predicate is also gated on
// `feature = "audio-in-rust"`. On a Windows `--features audio-capture`
// build (no `audio-in-rust`) the gate correctly stays false, so the
// picker DOES merge DirectSound endpoints — and on a machine with a
// DirectSound-only mic this test's "must be absent" assertion would
// fail for the right reason. Gate it to the configuration whose
// invariant it actually encodes.
#[cfg(all(windows, feature = "audio-in-rust"))]
#[test]
fn windows_picker_under_rust_capture_omits_directsound_only_endpoints() {
    // Companion pin for the DirectSound gating on the REAL picker
    // path. cpal 0.18 has no DirectSound host, so under Rust capture
    // the UI picker envelope
    // (`list_input_devices_for_ui_json_line`) MUST NOT advertise a
    // DirectSound-only endpoint — the user would pick a mic that
    // cannot be opened.
    //
    // `enumeration_flow(include_directsound=true, rust_capture=true)`
    // returns merge_directsound=false, so the merge is skipped. This
    // test verifies that gate end-to-end through the actual UI entry
    // point rather than the pure helper.
    use cpal::traits::HostTrait;

    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("VOICEPI_AUDIO_BACKEND");
    std::env::set_var("VOICEPI_AUDIO_BACKEND", "rust");

    let line = super::list_input_devices_for_ui_json_line();
    // Collect every name cpal can actually see, for the
    // "DirectSound-only" determination below.
    let mut cpal_names: Vec<String> = Vec::new();
    for host_id in cpal::available_hosts() {
        let Ok(host) = cpal::host_from_id(host_id) else {
            continue;
        };
        let Ok(devices) = host.input_devices() else {
            continue;
        };
        for device in devices {
            let n = device.to_string();
            if !n.trim().is_empty() {
                cpal_names.push(n);
            }
        }
    }
    let ds_names = super::directsound_capture_names_public();

    match prev {
        Some(v) => std::env::set_var("VOICEPI_AUDIO_BACKEND", v),
        None => std::env::remove_var("VOICEPI_AUDIO_BACKEND"),
    }

    let published: Vec<super::DeviceInfo> =
        serde_json::from_str(line.trim()).expect("picker envelope must be a valid JSON array");

    // A DirectSound-only endpoint = seen by DirectSound but by NO cpal
    // host. None of those may appear in the picker under Rust capture.
    for ds in &ds_names {
        let is_directsound_only = !cpal_names.iter().any(|c| super::name_matches(c, ds));
        if !is_directsound_only {
            continue;
        }
        assert!(
            !published.iter().any(|d| super::name_matches(&d.name, ds)),
            "picker published DirectSound-only endpoint {ds:?} under \
             VOICEPI_AUDIO_BACKEND=rust; cpal 0.18 cannot open it so \
             the merge must be gated off."
        );
    }
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

// The previous behavioral
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
// cpal at all. This test fails if the picker gate regresses and
// doesn't depend on live hardware — so it strictly supersedes the
// deleted live-enumeration test.
