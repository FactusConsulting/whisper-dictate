//! Pure capture-route gating and merge/publish predicates for the picker.
//!
//! Everything here is unit-testable without a live audio backend: the
//! predicates take their inputs as parameters so tests can cover feature,
//! env, and engine combinations the local build does not have.
/// Pure summary of the merge decisions [`enumerate_all_hosts`] makes,
/// given the caller's opt-in flag and whether the Rust capture backend
/// is active. Encodes the current decision matrix:
///
///   * `walk_non_default_hosts` — is the non-default cpal-host loop
///     invoked? This is unconditionally `true`; capture mode does not
///     suppress discovery of non-default hosts.
///   * `merge_directsound` — see [`should_merge_directsound_endpoints`].
///
/// Split out as a pure function so both properties are unit-testable
/// without a live cpal backend AND without touching the process
/// environment. The regression test for the picker fix asserts
/// `walk_non_default_hosts == true` for BOTH `rust_capture=true` and
/// `rust_capture=false`.
pub(crate) fn enumeration_flow(include_directsound: bool, rust_capture: bool) -> EnumerationFlow {
    EnumerationFlow {
        walk_non_default_hosts: true,
        merge_directsound: should_merge_directsound_endpoints(include_directsound, rust_capture),
    }
}

/// Result of [`enumeration_flow`] — the two boolean gates
/// [`enumerate_all_hosts`] consults after processing the default host.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EnumerationFlow {
    pub walk_non_default_hosts: bool,
    pub merge_directsound: bool,
}

/// Whether the running binary will ACTUALLY route capture through a
/// cpal-based Rust pipeline. There are TWO such routes and the picker
/// must apply its strict filter for BOTH:
///
/// * **Legacy worker-audio opt-in** — `VOICEPI_AUDIO_BACKEND=rust`
///   drives [`crate::runtime::audio_spawn::should_use_rust_audio_backend`].
/// * **In-process Rust engine (the shipping default)**
///   `VOICEPI_DICTATE_ENGINE` unset/empty/`rust` installs the
///   in-process runtime, whose push-to-talk capture lifecycle
///   ([`crate::runtime::rust_session_audio`]) opens
///   [`crate::audio::RawCapturePipeline`] (cpal) while recording, without
///   consulting `VOICEPI_AUDIO_BACKEND`. The shipping default configuration
///   therefore includes this route in the strict filter and suppresses the
///   DirectSound merge while cpal is the active capture path.
///
/// Both routes additionally require the `audio-capture` feature that makes
/// cpal capture available.
///
/// Split out so `enumerate_all_hosts` reads the state at most once
/// and so the pure gate helper never touches the process environment.
pub(super) fn current_backend_is_rust() -> bool {
    effective_rust_capture_gate(
        cfg!(feature = "audio-capture"),
        current_backend_env_requests_rust(),
        in_process_rust_engine_captures(),
    )
}

/// Whether the in-process Rust engine route will perform cpal capture.
///
/// Requires (a) every feature the route is gated on (see
/// [`in_process_capture_features_present`]) and (b) the engine choice
/// resolving to the only supported native runtime. Legacy or unknown engine
/// values fail during startup and must not reactivate a broader legacy
/// sounddevice device list.
///
/// ## Known limitation: static prediction, not observed install result
///
/// This is a COMPILE-TIME + ENV prediction of the installed capture
/// route, not the supervisor's observed
/// [`crate::runtime::in_process::try_install`] outcome. If a
/// feature-complete build attempts the in-process install and it fails
/// at RUNTIME (Whisper model missing, audio pipeline init error,
/// panic caught by `try_install`), the supervisor reports the error while
/// this predicate still reports `true`. The picker stays strict because no
/// alternate capture engine exists.
///
/// Not plumbed through because the observed result is not reachable
/// from both picker call sites:
///
/// * `ui::tasks` calls the picker in-process, so a supervisor-set
///   global WOULD be visible — but the picker also runs BEFORE any
///   install attempt (UI startup / "Refresh Devices" before the first
///   dictation), when no outcome exists yet.
/// * The `devices` CLI subcommand runs in a separate process and cannot
///   observe the parent supervisor's install result without new IPC.
///
/// Wiring only the in-process path would make the UI picker and the
/// `devices` CLI disagree, which is worse than a consistent static
/// rule. The residual window is narrow (install failed => Rust
/// dictation is already broken and the supervisor logs it) and errs
/// toward showing fewer devices rather than advertising unopenable
/// ones. Revisit if the supervisor ever gains a durable, env- or
/// IPC-propagated "effective backend" signal both call sites can read.
fn in_process_rust_engine_captures() -> bool {
    // NOTE: `dictate::mic` (RawCapturePipeline, gated on the looser
    // `audio-capture`) is deliberately NOT part of this condition
    // it is reachable only through the `dictate-mic` CLI verb in
    // `main.rs`, never through the dictation engine, so it does not
    // determine what the Settings picker should advertise.
    in_process_capture_features_present(
        cfg!(feature = "audio-capture"),
        cfg!(feature = "whisper-rs-local"),
        cfg!(feature = "rust-injection"),
        cfg!(feature = "rust-hotkeys"),
    )
}

/// Whether this build carries EVERY feature the in-process Rust
/// engine's cpal capture route needs. All four are required because
/// the route only exists when the whole chain is compiled in:
///
/// * `rust-hotkeys` + `rust-injection` — [`crate::runtime::in_process::try_install`]
///   is `#[cfg]`-gated on BOTH. Without either it is the stub that
///   returns `FeaturesMissing`, so native startup fails with an actionable
///   feature diagnostic.
/// * `whisper-rs-local` + `rust-injection` — gate
///   [`crate::runtime::rust_session_real_backends`], the parent of the
///   capture wiring.
/// * `audio-capture` — gates
///   [`crate::runtime::rust_session_audio`] itself, which opens a
///   [`crate::audio::RawCapturePipeline`] for each recording.
///
/// Takes the flags as parameters rather than reading `cfg!` inline so
/// the composition is unit-testable across feature combinations the
/// local build does not have.
pub(crate) fn in_process_capture_features_present(
    audio_capture: bool,
    whisper_rs_local: bool,
    rust_injection: bool,
    rust_hotkeys: bool,
) -> bool {
    audio_capture && whisper_rs_local && rust_injection && rust_hotkeys
}

/// Read the raw `VOICEPI_AUDIO_BACKEND` env var. Isolated from
/// `current_backend_is_rust` so [`effective_rust_capture_gate`] can be
/// unit-tested against synthetic inputs without touching process env.
fn current_backend_env_requests_rust() -> bool {
    std::env::var("VOICEPI_AUDIO_BACKEND")
        .ok()
        .map(|v| v.trim().eq_ignore_ascii_case("rust"))
        .unwrap_or(false)
}

/// Pure predicate: whether the picker's strict Rust-capture filter
/// should fire.
///
/// * `feature_available` — `audio-capture` compiled in. Without it no cpal
///   capture path exists.
/// * `env_requests_rust` — the legacy `VOICEPI_AUDIO_BACKEND=rust`
///   worker-audio opt-in.
/// * `in_process_engine_captures` — the in-process Rust engine (the
///   DEFAULT) will open `RawCapturePipeline` itself. This route never
///   consults `VOICEPI_AUDIO_BACKEND`, so it is included explicitly in the
///   gate for the shipping default configuration.
///
/// True when the feature is present AND *either* Rust-capture route is
/// active.
pub(crate) fn effective_rust_capture_gate(
    feature_available: bool,
    env_requests_rust: bool,
    in_process_engine_captures: bool,
) -> bool {
    feature_available && (env_requests_rust || in_process_engine_captures)
}

/// Whether `append_host_devices` should publish a device to the
/// picker, given:
///
/// * `max_input_channels` — from `probe_device_config`. Zero means
///   neither `supported_input_configs` nor `default_input_config`
///   reported a usable shape, so no backend can open it.
/// * `rust_capture_strict` — whether the Rust capture pipeline will
///   actually serve capture (see [`effective_rust_capture_gate`]).
/// * `supports_rust_capture` — whether
///   [`crate::audio::hosts::device_supports_rust_capture`] accepted
///   the device (i.e. `pick_config` can open it). Callers pass `false`
///   when `rust_capture_strict` is false, since the value is then
///   irrelevant and probing it would be wasted work.
///
/// Decision matrix (the behavioural seam exhaustively unit-tested in
/// `devices_tests.rs` WITHOUT
/// live audio hardware, so a headless CI runner still catches
/// regressions such as ignoring `rust_capture_strict`, inverting the
/// predicate, or hard-coding one return value):
///
/// | channels | strict | openable | publish |
/// |----------|--------|----------|---------|
/// | 0        | any    | any      | NO      |
/// | >0       | false  | any      | YES     |
/// | >0       | true   | false    | NO      |
/// | >0       | true   | true     | YES     |
pub(crate) fn should_publish_device(
    max_input_channels: u16,
    rust_capture_strict: bool,
    supports_rust_capture: bool,
) -> bool {
    if max_input_channels == 0 {
        return false;
    }
    if rust_capture_strict {
        return supports_rust_capture;
    }
    true
}

/// Whether the picker enumeration should merge Windows DirectSound-only
/// capture endpoints into the list.
///
/// * `include_directsound` — the caller's opt-in flag. The sounddevice
///   picker passes `true`; every cpal-based caller passes `false`
///   because cpal cannot open DirectSound endpoints.
/// * `rust_capture` — whether `VOICEPI_AUDIO_BACKEND=rust` is active,
///   i.e. the Rust capture path (cpal 0.18) will open selected devices.
///
/// Matrix:
///   * `include_directsound=false` → never merge (cpal callers).
///   * `include_directsound=true` + `rust_capture=false` → merge
///     (legacy sounddevice picker path).
///   * `include_directsound=true` + `rust_capture=true` → DO NOT merge:
///     the picker would advertise a mic the Rust capture path cannot
///     open. The non-default-host walk remains enabled in this mode so
///     ASIO/JACK/Pulse/PipeWire microphones are still discoverable; only
///     the DirectSound merge belongs under the gate.
pub(crate) fn should_merge_directsound_endpoints(
    include_directsound: bool,
    rust_capture: bool,
) -> bool {
    include_directsound && !rust_capture
}

#[cfg(test)]
mod tests {
    use super::{enumeration_flow, should_merge_directsound_endpoints};

    #[test]
    fn flow_and_predicate_agree_on_every_gate_combination() {
        for include_ds in [false, true] {
            for rust_capture in [false, true] {
                assert_eq!(
                    should_merge_directsound_endpoints(include_ds, rust_capture),
                    enumeration_flow(include_ds, rust_capture).merge_directsound,
                    "include_directsound={include_ds}, rust_capture={rust_capture}",
                );
            }
        }
        assert!(!should_merge_directsound_endpoints(false, false));
        assert!(should_merge_directsound_endpoints(true, false));
        assert!(!should_merge_directsound_endpoints(true, true));
    }
}
