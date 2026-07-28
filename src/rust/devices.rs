//! Rust port of `src/python/whisper_dictate/vp_devices.py` — input-device
//! enumeration for the microphone picker.
//!
//! The Python module enumerates audio inputs via PortAudio (sounddevice) and
//! collapses the WASAPI/DirectSound/MME/WDM-KS duplication PortAudio exposes
//! on Windows down to a single entry per physical mic. cpal already enumerates
//! devices through the preferred host backend on each platform (WASAPI on
//! Windows, ALSA on Linux, CoreAudio on macOS), so this Rust port is the
//! cheap-and-clean equivalent: one entry per cpal input device, with non-default
//! hosts merged behind so PulseAudio/PipeWire/JACK setups don't hide USB mics.
//!
//! **Windows DirectSound parity.** cpal is WASAPI-only on Windows, but the
//! sounddevice picker deliberately surfaces DirectSound-exclusive inputs (a
//! freshly docked/hot-plugged USB mic can appear on DirectSound before WASAPI).
//! To reach parity — the prerequisite for defaulting the picker to this helper —
//! [`enumerate_all_hosts`] also runs a native `DirectSoundCaptureEnumerateW`
//! pass on Windows and merges any DirectSound-only devices in by name (see
//! [`append_extra_named_devices`]). This is a no-op on other platforms.
//!
//! The module is gated behind the `audio-capture` cargo feature (cpal is the
//! only heavy native dep this pulls in, and the audio feature has the same
//! libasound requirement on Linux, so it makes sense to share the gate).
//!
//! Public API mirrors the shape the Settings UI / picker expects:
//!   * [`list_input_devices`] → `Vec<DeviceInfo>` (default flag set on the
//!     host's default input).
//!   * [`default_input_device`] → `Option<DeviceInfo>` for the platform
//!     default.
//!   * [`find_device_by_name`] → exact + longest-substring match, same
//!     precedence the Python resolver uses.
//!
//! The CLI subcommand `devices` (`handle_devices`) serialises the same list
//! as a JSON envelope so `vp_devices.py` can shell out to it when
//! `VOICEPI_DEVICES_BACKEND=rust` is set.

use std::io::{self, IsTerminal, Read};

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};

/// One enumerated input device, shaped to match the JSON contract the Python
/// picker emits (so the UI / shell-out keep working without translation).
///
/// `sample_rates` is the inclusive `(min, max)` range cpal reports for the
/// device's supported input configurations. Some backends only know a single
/// rate; those report `min == max`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Position in the default-host's `input_devices()` enumeration order
    /// (parallels cpal's own iteration so `nth(index)` in the capture path
    /// resolves the same physical device). Devices contributed by non-default
    /// hosts get indices appended after the default host's range; those entries
    /// are intended to be matched by NAME, not by numeric index.
    pub index: usize,
    /// Human-readable device name (cpal's `Display` impl on every backend).
    pub name: String,
    /// Maximum input channel count across the device's supported configs.
    /// Matches the sounddevice JSON contract (`max_input_channels`) the Python
    /// picker emits. Entries with zero usable input configs are filtered out
    /// upstream, so any value here is ≥ 1.
    pub max_input_channels: u16,
    /// `(min_hz, max_hz)` from cpal's supported-input-configs union.
    /// `(0, 0)` when the device exposes no input configs (extremely rare;
    /// defensive against backend quirks).
    pub sample_rates: (u32, u32),
    /// True when this entry IS the host's default input device (matched on
    /// the cpal-native index of the default host, so duplicate-named devices
    /// don't all carry the flag).
    pub default: bool,
}

/// Enumerate every input device the platform exposes. Default-host devices
/// come first in their cpal-native order; non-default-host devices are appended
/// behind (de-duplicated by name) so a saved mic exposed only via JACK/ASIO
/// still shows up in the picker. Devices with zero usable input configs or
/// blank names are filtered out so a caller can show the list verbatim.
pub fn list_input_devices() -> Vec<DeviceInfo> {
    enumerate_all_hosts(false)
}

/// Like [`list_input_devices`] but ALSO merges Windows DirectSound-only capture
/// devices (see [`append_extra_named_devices`]).
///
/// Reserved for the **sounddevice picker** — PortAudio can open DirectSound
/// inputs, so advertising them there is correct. cpal-based callers
/// (`dictate-mic`, the Rust audio self-test, the standalone `devices` CLI) must
/// use [`list_input_devices`] instead: cpal is WASAPI-only and a
/// DirectSound-exclusive device would fail to open, so a user must never be
/// offered one for a cpal capture path.
fn list_input_devices_with_directsound() -> Vec<DeviceInfo> {
    enumerate_all_hosts(true)
}

/// JSON envelope the desktop UI's Microphone picker consumes — a raw JSON array
/// of `{index, name, max_input_channels, default, …}` entries. Matches the
/// wire shape the Python worker's `--list-audio-devices` used to emit, so the
/// UI parser in [`crate::ui::parse_audio_devices_json`] stays authoritative.
///
/// Uses [`list_input_devices_with_directsound`] so a freshly docked/hot-plugged
/// mic visible only on Windows DirectSound still shows up in the picker (the
/// UI-side sounddevice equivalent). Empty result serialises as `"[]"`.
///
/// Exposed as `pub` so `ui::tasks::run_list_audio_devices` can call it in a
/// background thread and skip the Python subprocess. Result is a single JSON
/// line with a trailing newline so appending to a log stream keeps the same
/// shape a subprocess capture would have written.
pub fn list_input_devices_for_ui_json_line() -> String {
    let devices = list_input_devices_with_directsound();
    let mut out = serde_json::to_string(&devices).unwrap_or_else(|_| "[]".to_owned());
    out.push('\n');
    out
}

/// The host's default input device, if any. Returns the same `DeviceInfo`
/// shape so the UI can render it identically to the picker entries. The
/// `index` field reports the device's real position in [`list_input_devices`]
/// so callers comparing the two envelopes stay consistent.
pub fn default_input_device() -> Option<DeviceInfo> {
    let list = list_input_devices();
    list.into_iter().find(|d| d.default)
}

/// Find a device by name. Precedence matches the Python resolver:
///   1. case-insensitive EXACT name match wins,
///   2. otherwise case-insensitive SUBSTRING match (bidirectional — saved
///      name in device name, or device name in saved value — so an
///      MME-truncated saved value still maps to its full WASAPI name),
///      preferring the LONGEST matching device name so a truncated saved
///      value binds to the fullest sibling rather than a generic prefix.
///
/// Returns `None` if no device matches.
pub fn find_device_by_name(query: &str) -> Option<DeviceInfo> {
    let devices = list_input_devices();
    find_in(&devices, query).cloned()
}

// ----- pure helpers (unit-testable without a real cpal host) ------------------

/// Pure name lookup. Exposed so the test suite can exercise it against a
/// hand-rolled device list without depending on a live audio backend.
///
/// See [`find_device_by_name`] for the precedence rules; the longest-substring
/// tie-breaker matters because PortAudio's MME path truncates names to 31
/// chars, and a saved MME value must bind to its full WASAPI sibling — not to
/// a generic prefix like "Microphone".
pub fn find_in<'a>(devices: &'a [DeviceInfo], query: &str) -> Option<&'a DeviceInfo> {
    let needle = query.trim();
    if needle.is_empty() {
        return None;
    }
    let folded = needle.to_lowercase();
    // 1. exact case-insensitive match wins
    if let Some(hit) = devices.iter().find(|d| d.name.to_lowercase() == folded) {
        return Some(hit);
    }
    // 2. bidirectional substring match — same semantics as
    //    vp_devices._name_matches: either side may be the prefix. Iterate the
    //    whole list and keep the entry with the LONGEST matching name; the
    //    Python resolver (vp_devices.resolve_capture_device._best_match) does
    //    the same so a truncated MME saved value still maps to the fullest
    //    WASAPI sibling rather than to a shorter generic match.
    let mut best: Option<&DeviceInfo> = None;
    for d in devices {
        let lower = d.name.to_lowercase();
        if lower.is_empty() {
            continue;
        }
        if !(lower.contains(&folded) || folded.contains(&lower)) {
            continue;
        }
        match best {
            None => best = Some(d),
            Some(prev) if d.name.len() > prev.name.len() => best = Some(d),
            _ => {}
        }
    }
    best
}

/// Walk every cpal host (default first), enumerate input devices on each, and
/// merge them into a single list de-duplicated by name. The default host's
/// entries keep their cpal-native indices so the capture path's numeric-index
/// selector still resolves the same physical device.
///
/// The Rust capture path (`VOICEPI_AUDIO_BACKEND=rust`) walks the same host
/// list as this enumeration via [`crate::audio::hosts::resolve_input`], so
/// non-default cpal hosts (ASIO, JACK, PipeWire, Pulse) are safe to advertise
/// — capture will pick whichever host actually exposes the selected name.
/// Windows DirectSound is still skipped under Rust capture, though: cpal 0.18
/// has no DirectSound host, so a DirectSound-only mic in the picker would
/// fail to open. The Python `audio-in-python` path can open DirectSound, so
/// the merge stays on for it.
fn enumerate_all_hosts(include_directsound: bool) -> Vec<DeviceInfo> {
    let default_host = cpal::default_host();
    let default_host_id = default_host.id();
    let default_input_index = default_input_index(&default_host);
    let rust_capture = current_backend_is_rust();

    let mut out: Vec<DeviceInfo> = Vec::new();
    let mut seen_names: Vec<String> = Vec::new();
    append_host_devices(
        &default_host,
        /*default_input_index=*/ default_input_index,
        /*is_default_host=*/ true,
        /*rust_capture_strict=*/ rust_capture,
        /*next_synthetic_index=*/ &mut 0,
        &mut out,
        &mut seen_names,
    );

    let flow = enumeration_flow(include_directsound, rust_capture);
    if flow.walk_non_default_hosts {
        append_non_default_host_devices(default_host_id, rust_capture, &mut out, &mut seen_names);
    }
    if flow.merge_directsound {
        append_extra_named_devices(&directsound_capture_names(), &mut out, &mut seen_names);
    }

    out
}

/// Walk every cpal host EXCEPT `default_host_id` and append their input
/// devices to `out`. Split out so [`enumerate_all_hosts`] can express
/// its decision matrix at one abstraction level AND so the "walked
/// unconditionally" invariant (Codex P2 on `hosts.rs:129`, PR #663) can
/// be pinned via [`enumeration_flow`] independently of the live cpal
/// enumeration.
fn append_non_default_host_devices(
    default_host_id: cpal::HostId,
    rust_capture: bool,
    out: &mut Vec<DeviceInfo>,
    seen_names: &mut Vec<String>,
) {
    for host_id in cpal::available_hosts() {
        if host_id == default_host_id {
            continue;
        }
        let Ok(host) = cpal::host_from_id(host_id) else {
            continue;
        };
        // Synthetic index starts AFTER the highest cpal-native index already
        // in `out`, not just after `out.len()`. The default host may have gaps
        // (blank-name or zero-channel devices were skipped) so `out.len()` can
        // be lower than the max native index and cause a collision.
        let mut next_synthetic = next_synthetic_from(out);
        append_host_devices(
            &host,
            /*default_input_index=*/ None,
            /*is_default_host=*/ false,
            /*rust_capture_strict=*/ rust_capture,
            &mut next_synthetic,
            out,
            seen_names,
        );
    }
}

/// Pure summary of the merge decisions [`enumerate_all_hosts`] makes,
/// given the caller's opt-in flag and whether the Rust capture backend
/// is active. Encodes the FULL post-fix decision matrix:
///
///   * `walk_non_default_hosts` — is the non-default cpal-host loop
///     invoked? Post-fix this is UNCONDITIONALLY `true`; pre-#663 the
///     code returned early under `rust_capture` and this was `false`.
///   * `merge_directsound` — see [`should_merge_directsound_endpoints`].
///
/// Split out as a pure function so both properties are unit-testable
/// without a live cpal backend AND without touching the process
/// environment. The regression test for the picker fix asserts
/// `walk_non_default_hosts == true` for BOTH `rust_capture=true` and
/// `rust_capture=false`, which the pre-#663 code would have failed.
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

/// Whether the running binary will ACTUALLY route capture through the
/// Rust pipeline. Mirrors [`crate::runtime::audio_spawn::should_use_rust_audio_backend`]:
/// requires BOTH the `VOICEPI_AUDIO_BACKEND=rust` env var AND the
/// `audio-in-rust` cargo feature. On an `audio-capture`-only build the
/// supervisor falls back to Python sounddevice regardless of the env
/// var, so filtering the picker with the strict pick-config
/// requirement would prune U16-only / `default_input_config`-only
/// microphones that the effective Python backend CAN open — Codex P2
/// (#674 devices.rs:206).
///
/// Split out so [`enumerate_all_hosts`] reads the state at most once
/// and so the pure merge-gate helper never touches the process
/// environment.
fn current_backend_is_rust() -> bool {
    effective_rust_capture_gate(
        cfg!(feature = "audio-in-rust"),
        current_backend_env_requests_rust(),
    )
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
/// should fire. Only true when the running binary CAN actually route
/// capture through the Rust pipeline (feature compiled in) AND the
/// user asked for it (env var set). Otherwise the effective backend
/// is Python sounddevice, which handles more formats than
/// `capture::pick_config`, and the strict filter would over-prune —
/// Codex P2 (#674 devices.rs:206).
pub(crate) fn effective_rust_capture_gate(
    feature_available: bool,
    env_requests_rust: bool,
) -> bool {
    feature_available && env_requests_rust
}

/// Whether [`append_host_devices`] should publish a device to the
/// picker, given:
///
/// * `max_input_channels` — from [`probe_device_config`]. Zero means
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
/// Decision matrix (the behavioural seam Codex P2 #674 devices.rs:600
/// asked for — exhaustively unit-tested in `devices_tests.rs` WITHOUT
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
///     (Python sounddevice picker path).
///   * `include_directsound=true` + `rust_capture=true` → DO NOT merge:
///     the picker would advertise a mic the Rust capture path cannot
///     open. This is the Codex P2 case on `hosts.rs:129` (PR #663) —
///     the pre-fix code short-circuited the entire non-default-host
///     walk under `rust_capture`, which ALSO hid ASIO/JACK/Pulse/PipeWire
///     mics from the picker; only the DirectSound merge belongs under
///     the gate.
pub(crate) fn should_merge_directsound_endpoints(
    include_directsound: bool,
    rust_capture: bool,
) -> bool {
    include_directsound && !rust_capture
}

/// Returns the first synthetic index to use for non-default-host devices:
/// `max(reported index) + 1` so synthetic indices never collide with the
/// default host's cpal-native indices even when the native range is sparse.
pub(crate) fn next_synthetic_from(devices: &[DeviceInfo]) -> usize {
    devices.iter().map(|d| d.index).max().map_or(0, |m| m + 1)
}

/// Case-insensitive bidirectional substring match — either string may contain
/// the other. Mirrors `vp_devices._name_matches`. Used to de-duplicate
/// DirectSound names against WASAPI names for the same physical mic, since the
/// two Windows host APIs annotate the same device slightly differently.
pub(crate) fn name_matches(a: &str, b: &str) -> bool {
    let a = a.trim().to_lowercase();
    let b = b.trim().to_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.contains(&b) || b.contains(&a)
}

/// Merge externally-enumerated capture-device names (currently Windows
/// DirectSound) into `out`, skipping any that already correspond to a device
/// cpal reported.
///
/// cpal on Windows enumerates WASAPI only, but the sounddevice picker
/// deliberately surfaces DirectSound-only inputs — a freshly docked/hot-plugged
/// USB mic can be visible on DirectSound before it appears on WASAPI. Without
/// this merge those mics would silently vanish from the picker once it defaults
/// to the Rust helper, even though the sounddevice capture path can still open
/// them. De-duplication uses the bidirectional-substring rule (not an exact
/// match) because DirectSound and WASAPI report slightly different name strings
/// for the same physical device.
///
/// Appended entries get synthetic indices after the existing range and a
/// nominal channel count: they exist so the NAME reaches the picker, and the
/// sounddevice capture path resolves the real device (and its true channel /
/// sample-rate shape) from that name. This mirrors how the picker already
/// treats non-default-host entries as name-addressable, not index-addressable.
pub(crate) fn append_extra_named_devices(
    extra: &[String],
    out: &mut Vec<DeviceInfo>,
    seen_names: &mut Vec<String>,
) {
    let mut next = next_synthetic_from(out);
    for name in extra {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen_names.iter().any(|seen| name_matches(seen, trimmed)) {
            continue;
        }
        out.push(DeviceInfo {
            index: next,
            name: trimmed.to_owned(),
            max_input_channels: 1,
            sample_rates: (0, 0),
            default: false,
        });
        seen_names.push(trimmed.to_owned());
        next += 1;
    }
}

/// Enumerate Windows DirectSound *capture* device descriptions.
///
/// cpal is WASAPI-only on Windows, so this native `DirectSoundCaptureEnumerateW`
/// pass is the only way to see DirectSound-exclusive inputs (see
/// [`append_extra_named_devices`]). Best-effort: any failure yields an empty
/// list, so the picker degrades to the WASAPI set rather than erroring. On
/// non-Windows targets there is no DirectSound, so this is a no-op returning an
/// empty list.
#[cfg(windows)]
fn directsound_capture_names() -> Vec<String> {
    directsound::capture_device_names()
}

#[cfg(not(windows))]
fn directsound_capture_names() -> Vec<String> {
    Vec::new()
}

/// Public wrapper so `audio::hosts::directsound_only_hint` can consult the
/// same enumeration without duplicating the FFI shim. Same shape as the
/// private helper — Windows returns the `DirectSoundCaptureEnumerateW`
/// results, every other target returns an empty vector.
///
/// Callers should treat this as a diagnostic aid only: cpal 0.18 cannot
/// open DirectSound endpoints, so a name that appears here but nowhere in
/// cpal's WASAPI/ASIO enumeration is a mic the capture path cannot use.
pub fn directsound_capture_names_public() -> Vec<String> {
    directsound_capture_names()
}

#[cfg(windows)]
mod directsound {
    use std::ffi::c_void;

    use windows::core::{BOOL, GUID, PCWSTR};
    use windows::Win32::Media::Audio::DirectSound::DirectSoundCaptureEnumerateW;

    /// C callback invoked once per DirectSound capture device. `context` is a
    /// `*mut Vec<String>` we thread through so each description is collected.
    /// The first callback carries a NULL GUID (the "Primary Sound Capture
    /// Driver" alias for the default device); we keep it too, since the picker
    /// de-duplicates by name against the WASAPI list anyway.
    unsafe extern "system" fn enum_callback(
        guid: *mut GUID,
        description: PCWSTR,
        _module: PCWSTR,
        context: *mut c_void,
    ) -> BOOL {
        // The first callback carries a NULL GUID: the "Primary Sound Capture
        // Driver" alias for the system default. It has no stable physical-device
        // name and, since it can't match a real WASAPI entry, would surface as a
        // redundant picker option that merely re-selects the default. Skip it —
        // the Python DirectSound path filters this alias too.
        if !guid.is_null() && !context.is_null() && !description.is_null() {
            // SAFETY: `context` is the `&mut Vec<String>` we passed to
            // DirectSoundCaptureEnumerateW; the enumeration is synchronous so
            // the borrow is valid for the duration of every callback.
            let names = unsafe { &mut *(context as *mut Vec<String>) };
            if let Ok(text) = unsafe { description.to_string() } {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    names.push(trimmed.to_owned());
                }
            }
        }
        // TRUE → continue enumerating the remaining devices.
        BOOL(1)
    }

    pub(super) fn capture_device_names() -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let context = &mut names as *mut Vec<String> as *mut c_void;
        // SAFETY: `enum_callback` matches the LPDSENUMCALLBACKW signature and
        // `context` outlives the synchronous enumeration call.
        unsafe {
            let _ = DirectSoundCaptureEnumerateW(Some(enum_callback), Some(context));
        }
        // Opt-in diagnostic (`VOICEPI_DEBUG_DIRECTSOUND=1`): report the raw
        // DirectSound capture names on stderr BEFORE the picker de-duplicates
        // them against the WASAPI list. In steady state these mirror the cpal
        // devices (so they dedupe away); this makes it possible to confirm the
        // enumeration actually returned devices rather than silently finding
        // nothing. stderr keeps the JSON stdout envelope clean.
        if std::env::var("VOICEPI_DEBUG_DIRECTSOUND")
            .map(|v| !matches!(v.trim(), "" | "0" | "false" | "no" | "off"))
            .unwrap_or(false)
        {
            eprintln!(
                "[devices:directsound] enumerated {} capture device(s): {:?}",
                names.len(),
                names
            );
        }
        names
    }
}

/// Look up the default input device's index inside the host's `input_devices()`
/// enumeration. Returns `None` when the host has no default input OR the
/// default can't be located by name in the device list (defensive against
/// backend quirks that return a default but enumerate it differently).
fn default_input_index(host: &cpal::Host) -> Option<usize> {
    let default = host.default_input_device()?;
    let default_name = default.to_string();
    if default_name.trim().is_empty() {
        return None;
    }
    let iter = host.input_devices().ok()?;
    for (idx, device) in iter.enumerate() {
        if device.to_string() == default_name {
            return Some(idx);
        }
    }
    None
}

/// Enumerate a single host's input devices and append usable entries to `out`.
///
/// Falls back to enumerating just the host's default input device when
/// `input_devices()` itself fails — the Python picker never silently empties
/// the list when the backend is flaky, and the Settings UI relies on at least
/// the default mic appearing.
fn append_host_devices(
    host: &cpal::Host,
    default_input_index: Option<usize>,
    is_default_host: bool,
    rust_capture_strict: bool,
    next_synthetic_index: &mut usize,
    out: &mut Vec<DeviceInfo>,
    seen_names: &mut Vec<String>,
) {
    let iter = match host.input_devices() {
        Ok(iter) => iter,
        Err(err) => {
            // Backend hiccup (audio server restart, transient ALSA error, …).
            // Don't silently report an empty list — the picker would render
            // "no microphones" even though the OS clearly has at least a
            // default. Fall back to just that default with a logged warning.
            eprintln!(
                "[devices] host {:?} input_devices() failed: {err}; falling back to default input",
                host.id()
            );
            if is_default_host {
                if let Some(default) = host.default_input_device() {
                    let name = default.to_string();
                    if !name.trim().is_empty()
                        && !seen_names.iter().any(|n| n.eq_ignore_ascii_case(&name))
                    {
                        let info = build_device_info(0, &default, &name, true);
                        // Same publish decision as the main enumeration
                        // branch — otherwise the fallback could publish
                        // a device `pick_config` cannot open (Codex P2
                        // #669 devices.rs:271).
                        if should_publish_device(
                            info.max_input_channels,
                            rust_capture_strict,
                            rust_capture_strict
                                && crate::audio::hosts::device_supports_rust_capture(&default),
                        ) {
                            seen_names.push(name);
                            out.push(info);
                            *next_synthetic_index = out.len();
                        }
                    }
                }
            }
            return;
        }
    };

    for (cpal_index, device) in iter.enumerate() {
        let name = device.to_string();
        if name.trim().is_empty() {
            // Empty names collide with the Python UI's "(System default)"
            // sentinel, so we drop them just like select_input_devices does.
            continue;
        }
        // De-duplicate across hosts BY NAME. On Windows the default host
        // (WASAPI) already collapses host-API duplication, but cross-host
        // enumeration can re-introduce the same physical mic (e.g. ALSA
        // direct + Pulse default on Linux). The Python picker uses the same
        // bidirectional-substring rule for picker de-dup — we keep it simple
        // here with an exact case-insensitive name comparison, which already
        // covers the same-physical-device case.
        if seen_names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            continue;
        }

        let is_default = is_default_host && Some(cpal_index) == default_input_index;
        // Default-host entries keep their cpal-native index so the capture
        // path's `nth(index)` resolves the same physical device. Non-default
        // hosts get synthetic indices appended after the default host's range.
        let reported_index = if is_default_host {
            cpal_index
        } else {
            *next_synthetic_index
        };
        let info = build_device_info(reported_index, &device, &name, is_default);
        // Publish decision (channels > 0, plus the strict pick-config
        // contract under Rust capture) lives in the pure
        // [`should_publish_device`] helper so the full matrix is
        // unit-testable without live audio hardware — Codex P2
        // (#669 devices.rs:271, #674 devices.rs:600).
        if !should_publish_device(
            info.max_input_channels,
            rust_capture_strict,
            // Only probed when the strict gate is actually active, so
            // the non-strict path keeps its previous cost profile.
            rust_capture_strict && crate::audio::hosts::device_supports_rust_capture(&device),
        ) {
            continue;
        }
        seen_names.push(name);
        out.push(info);
        if !is_default_host {
            *next_synthetic_index += 1;
        }
    }
}

fn build_device_info(index: usize, device: &cpal::Device, name: &str, default: bool) -> DeviceInfo {
    let (channels, sample_rates) = probe_device_config(device);
    DeviceInfo {
        index,
        name: name.to_owned(),
        max_input_channels: channels,
        sample_rates,
        default,
    }
}

/// Inspect a cpal `Device` for its channel count and sample-rate range.
///
/// Uses `supported_input_configs()` as the source of truth (max channels and
/// the union of rate ranges), falling back to `default_input_config()` when
/// the supported-configs iterator is unavailable. This MUST NOT drop devices
/// where only `default_input_config()` errors but `supported_input_configs()`
/// still reports usable shapes — the capture path opens from the supported
/// list, so hiding such mics here is a UX regression.
fn probe_device_config(device: &cpal::Device) -> (u16, (u32, u32)) {
    let mut max_channels: u16 = 0;
    let mut lo: u32 = 0;
    let mut hi: u32 = 0;

    if let Ok(supported) = device.supported_input_configs() {
        for sc in supported {
            let ch = sc.channels();
            if ch > max_channels {
                max_channels = ch;
            }
            let smin: u32 = sc.min_sample_rate();
            let smax: u32 = sc.max_sample_rate();
            if smin > 0 && (lo == 0 || smin < lo) {
                lo = smin;
            }
            if smax > hi {
                hi = smax;
            }
        }
    }

    // If supported_input_configs returned nothing usable, try the default
    // config as a last resort. (Some backends only expose a single default
    // shape; supported_input_configs may still err on disconnected devices.)
    if max_channels == 0 {
        if let Ok(cfg) = device.default_input_config() {
            max_channels = cfg.channels();
            let r: u32 = cfg.sample_rate();
            lo = r;
            hi = r;
        }
    }

    (max_channels, (lo, hi))
}

// ----- CLI handler ------------------------------------------------------------

/// JSON request envelope for the hidden `devices` sub-command. Mirrors the
/// shape `handle_health` uses (action-tagged enum) so the Python shell-out
/// can pick the operation it wants without parsing multiple positional args.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum DevicesRequest {
    /// List every input device. `include_directsound` (default false) asks for
    /// the Windows DirectSound-only inputs to be merged in — set ONLY by the
    /// sounddevice picker, which can open them; the standalone CLI / TTY / empty
    /// callers leave it false so every listed device is cpal-openable.
    List {
        #[serde(default)]
        include_directsound: bool,
    },
    /// Return the host's default input device (or `null`).
    Default,
    /// Resolve a saved name against the live device list.
    Find { query: String },
}

#[derive(Debug, Serialize)]
struct ListResponse {
    devices: Vec<DeviceInfo>,
}

#[derive(Debug, Serialize)]
struct DefaultResponse {
    device: Option<DeviceInfo>,
}

#[derive(Debug, Serialize)]
struct FindResponse {
    device: Option<DeviceInfo>,
}

/// Pure resolver for [`handle_devices`]. Given whether stdin is a TTY and,
/// when it isn't, the raw stdin body, decide which [`DevicesRequest`] to
/// serve. Split out so the TTY / pipe / bad-JSON branches are unit-testable
/// without a real console or a piped subprocess.
///
/// Contract:
/// * `stdin_is_tty = true` → always [`DevicesRequest::List`] (the interactive
///   convenience — see [`handle_devices`] doc for why).
/// * `stdin_is_tty = false` + empty body → [`DevicesRequest::List`]
///   (documented shorthand for the Python shell-out).
/// * `stdin_is_tty = false` + non-empty body → parse as JSON, propagate the
///   parse error.
fn resolve_devices_request(stdin_is_tty: bool, stdin_body: Option<&str>) -> Result<DevicesRequest> {
    if stdin_is_tty {
        return Ok(DevicesRequest::List {
            include_directsound: false,
        });
    }
    let trimmed = stdin_body.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(DevicesRequest::List {
            include_directsound: false,
        });
    }
    Ok(serde_json::from_str(trimmed)?)
}

/// Handler for the hidden `devices` sub-command. Reads a JSON request from
/// stdin and writes a JSON response on stdout.
///
/// Accepts an empty / missing stdin body as a shorthand for
/// `{"action":"list"}` so callers that just want the list can pipe nothing in.
///
/// When stdin is an interactive TTY (nothing piped in) we skip the blocking
/// read entirely and default to `List` — otherwise a user typing
/// `whisper-dictate devices` from PowerShell would see the process hang
/// waiting for keyboard input until they hit Ctrl+Z. The Python shell-out and
/// `... | whisper-dictate devices` pipelines still hit the read path because
/// their stdin is not a TTY.
pub fn handle_devices() -> Result<()> {
    let stdin = io::stdin();
    let stdin_is_tty = stdin.is_terminal();
    let raw = if stdin_is_tty {
        None
    } else {
        let mut buf = String::new();
        stdin.lock().read_to_string(&mut buf)?;
        Some(buf)
    };
    let request = resolve_devices_request(stdin_is_tty, raw.as_deref())?;
    match request {
        DevicesRequest::List {
            include_directsound,
        } => {
            let devices = if include_directsound {
                list_input_devices_with_directsound()
            } else {
                list_input_devices()
            };
            let resp = ListResponse { devices };
            println!("{}", serde_json::to_string(&resp)?);
        }
        DevicesRequest::Default => {
            let resp = DefaultResponse {
                device: default_input_device(),
            };
            println!("{}", serde_json::to_string(&resp)?);
        }
        DevicesRequest::Find { query } => {
            let resp = FindResponse {
                device: find_device_by_name(&query),
            };
            println!("{}", serde_json::to_string(&resp)?);
        }
    }
    Ok(())
}

// ----- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make(index: usize, name: &str, default: bool) -> DeviceInfo {
        DeviceInfo {
            index,
            name: name.to_owned(),
            max_input_channels: 1,
            sample_rates: (16_000, 48_000),
            default,
        }
    }

    #[test]
    fn find_in_empty_query_returns_none() {
        let devs = vec![make(0, "Microphone", false)];
        assert!(find_in(&devs, "").is_none());
        assert!(find_in(&devs, "   ").is_none());
    }

    #[test]
    fn find_in_exact_match_wins_over_substring() {
        // "Microphone" is a clean prefix of "Microphone Array" — the exact
        // hit must bind to the second entry, not the longer sibling.
        let devs = vec![
            make(0, "Microphone Array", false),
            make(1, "Microphone", false),
        ];
        let hit = find_in(&devs, "Microphone").expect("exact match");
        assert_eq!(hit.index, 1);
    }

    #[test]
    fn find_in_is_case_insensitive() {
        let devs = vec![make(0, "Headset Microphone (Jabra Evolve 65 TE)", false)];
        let hit = find_in(&devs, "HEADSET microphone (jabra evolve 65 te)").expect("hit");
        assert_eq!(hit.index, 0);
    }

    #[test]
    fn find_in_substring_match_either_direction() {
        // Saved name is the truncated MME 31-char value; device name is the
        // full WASAPI name. The bidirectional substring rule must still match.
        let devs = vec![make(0, "Headset Microphone (Jabra Evolve 65 TE)", false)];
        let saved = "Headset Microphone (Jabra Evolv"; // truncated to 31 chars
        let hit = find_in(&devs, saved).expect("truncated match");
        assert_eq!(hit.index, 0);

        // Reverse direction: saved is longer than device name.
        let devs2 = vec![make(0, "Microphone", false)];
        let saved_long = "Microphone (Realtek)";
        let hit2 = find_in(&devs2, saved_long).expect("longer-saved match");
        assert_eq!(hit2.index, 0);
    }

    #[test]
    fn find_in_prefers_longest_substring_match() {
        // Regression for the truncated-MME hijack bug: when a saved value is
        // a substring of MULTIPLE device names, we must bind to the LONGEST
        // (fullest) one — not the first match in iteration order. Without
        // this, a saved "Headset Microphone (Jabra Evolv" would resolve to
        // the generic "Headset Microphone" sibling and capture would record
        // from the wrong physical device.
        let devs = vec![
            make(0, "Headset Microphone", false),
            make(1, "Headset Microphone (Jabra Evolve 65 TE)", false),
            make(2, "Headset Microphone (USB)", false),
        ];
        let saved = "Headset Microphone (Jabra Evolv"; // truncated MME
        let hit = find_in(&devs, saved).expect("longest match");
        assert_eq!(hit.index, 1);
    }

    #[test]
    fn find_in_returns_none_when_no_match() {
        let devs = vec![make(0, "Built-in Microphone", false)];
        assert!(find_in(&devs, "Webcam").is_none());
    }

    #[test]
    fn device_info_round_trips_as_json() {
        let dev = make(2, "Mic 2", true);
        let json = serde_json::to_string(&dev).unwrap();
        let back: DeviceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dev);
    }

    #[test]
    fn list_response_serialises_field_name() {
        let resp = ListResponse {
            devices: vec![make(0, "Mic", false)],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            json.contains("\"devices\""),
            "expected `devices` envelope key in {json}"
        );
    }

    #[test]
    fn devices_request_parses_list_action() {
        let parsed: DevicesRequest = serde_json::from_str("{\"action\":\"list\"}").unwrap();
        // Absent flag defaults to false → no DirectSound merge for plain callers.
        assert!(matches!(
            parsed,
            DevicesRequest::List {
                include_directsound: false
            }
        ));
    }

    #[test]
    fn devices_request_parses_list_with_directsound_flag() {
        // The sounddevice picker opts into the DirectSound merge explicitly.
        let parsed: DevicesRequest =
            serde_json::from_str("{\"action\":\"list\",\"include_directsound\":true}").unwrap();
        assert!(matches!(
            parsed,
            DevicesRequest::List {
                include_directsound: true
            }
        ));
    }

    #[test]
    fn devices_request_parses_find_action() {
        let parsed: DevicesRequest =
            serde_json::from_str("{\"action\":\"find\",\"query\":\"jabra\"}").unwrap();
        match parsed {
            DevicesRequest::Find { query } => assert_eq!(query, "jabra"),
            other => panic!("expected Find, got {other:?}"),
        }
    }

    // ----- resolve_devices_request: TTY vs pipe dispatch (Codex PR #564 P2) --

    #[test]
    fn resolve_defaults_to_list_when_stdin_is_a_tty() {
        // An interactive `whisper-dictate devices` in PowerShell has no
        // piped body — we skip the blocking read and default to the list so
        // the user sees output instead of the process hanging on stdin.
        let request = resolve_devices_request(true, None).unwrap();
        assert!(matches!(request, DevicesRequest::List { .. }));
    }

    #[test]
    fn resolve_defaults_to_list_when_stdin_is_a_tty_even_with_body() {
        // Defensive: if a caller ever passes a body while claiming TTY,
        // the TTY branch still wins (matches the doc contract — TTY means
        // interactive convenience regardless of the body).
        let request =
            resolve_devices_request(true, Some(r#"{"action":"find","query":"x"}"#)).unwrap();
        assert!(matches!(request, DevicesRequest::List { .. }));
    }

    #[test]
    fn resolve_defaults_to_list_when_piped_stdin_is_empty() {
        // The Python shell-out sometimes pipes nothing and expects a list —
        // this is the documented shorthand for `{"action":"list"}`.
        assert!(matches!(
            resolve_devices_request(false, Some("")).unwrap(),
            DevicesRequest::List { .. }
        ));
        assert!(matches!(
            resolve_devices_request(false, Some("   \n  ")).unwrap(),
            DevicesRequest::List { .. }
        ));
        assert!(matches!(
            resolve_devices_request(false, None).unwrap(),
            DevicesRequest::List { .. }
        ));
    }

    #[test]
    fn resolve_parses_piped_json_body() {
        // The Python shell-out for a name lookup passes a `find` envelope.
        let body = r#"{"action":"find","query":"jabra"}"#;
        let request = resolve_devices_request(false, Some(body)).unwrap();
        match request {
            DevicesRequest::Find { query } => assert_eq!(query, "jabra"),
            other => panic!("expected Find, got {other:?}"),
        }
    }

    #[test]
    fn resolve_returns_error_on_invalid_piped_json() {
        // A malformed body from a broken caller must surface an error, not
        // be silently swallowed as `List` (that would mask a broken
        // integration where the Python side thought it was asking for
        // something specific and got the wrong answer).
        let err = resolve_devices_request(false, Some("{not-json")).unwrap_err();
        // The exact wording is serde_json's business (it varies with the
        // input); just assert we surfaced SOMETHING with position info.
        let msg = err.to_string();
        assert!(!msg.is_empty(), "empty error message");
        assert!(
            msg.contains("line") || msg.contains("column"),
            "expected serde parse position info, got: {msg}"
        );
    }

    // ----- finding #3: synthetic index based on max reported index -----------

    #[test]
    fn next_synthetic_from_empty_is_zero() {
        assert_eq!(next_synthetic_from(&[]), 0);
    }

    #[test]
    fn next_synthetic_from_contiguous() {
        let devs = vec![make(0, "A", false), make(1, "B", false), make(2, "C", true)];
        assert_eq!(next_synthetic_from(&devs), 3);
    }

    #[test]
    fn next_synthetic_from_sparse_default_host_indices() {
        // cpal indices 0 and 5 with a gap (1..4 were blank/zero-channel and
        // skipped). out.len() == 2 but max index == 5; the first synthetic
        // index must be 6, not 2, to avoid colliding with native index 5.
        let devs = vec![make(0, "Mic A", false), make(5, "Mic B", true)];
        assert_eq!(next_synthetic_from(&devs), 6);
    }

    // ----- DirectSound merge (Windows parity, cross-platform-testable) --------

    #[test]
    fn name_matches_is_bidirectional_and_case_insensitive() {
        assert!(name_matches("Microphone (Realtek)", "microphone (realtek)"));
        // WASAPI full name contains the DirectSound truncation and vice versa.
        assert!(name_matches(
            "Headset Microphone (Jabra Evolve 65 TE)",
            "Headset Microphone"
        ));
        assert!(name_matches(
            "Headset Microphone",
            "Headset Microphone (Jabra Evolve 65 TE)"
        ));
        assert!(!name_matches("Webcam Mic", "Headset Microphone"));
        assert!(!name_matches("", "anything"));
        assert!(!name_matches("anything", "   "));
    }

    #[test]
    fn append_extra_adds_directsound_only_devices() {
        // WASAPI reported one mic; DirectSound also sees a freshly-docked USB
        // mic WASAPI hasn't surfaced yet. That one must be appended; the
        // already-present one (same physical device, slightly different name)
        // must be de-duplicated away.
        let mut out = vec![make(0, "Headset Microphone (Jabra Evolve 65 TE)", true)];
        let mut seen = vec!["Headset Microphone (Jabra Evolve 65 TE)".to_owned()];
        let ds = vec![
            "Headset Microphone (Jabra Evolve 65 TE)".to_owned(), // dup of WASAPI
            "Microphone (USB Docking Station)".to_owned(),        // DirectSound-only
            "   ".to_owned(),                                     // blank → skipped
        ];
        append_extra_named_devices(&ds, &mut out, &mut seen);

        assert_eq!(out.len(), 2);
        let added = &out[1];
        assert_eq!(added.name, "Microphone (USB Docking Station)");
        assert_eq!(added.index, 1); // synthetic index after the WASAPI range
        assert!(!added.default);
        assert!(added.max_input_channels >= 1);
    }

    #[test]
    fn append_extra_dedups_truncated_directsound_name() {
        // DirectSound truncates/annotates differently; the bidirectional
        // substring rule must treat it as the same device and NOT re-add it.
        let mut out = vec![make(0, "Microphone (Realtek(R) Audio)", true)];
        let mut seen = vec!["Microphone (Realtek(R) Audio)".to_owned()];
        let ds = vec!["Microphone (Realtek(R) Aud".to_owned()]; // truncated
        append_extra_named_devices(&ds, &mut out, &mut seen);
        assert_eq!(
            out.len(),
            1,
            "truncated DirectSound name must not duplicate the WASAPI entry"
        );
    }

    #[test]
    fn list_input_devices_for_ui_json_line_shape_is_a_parseable_array_with_trailing_newline() {
        // The UI parser (parse_audio_devices_json) tolerates surrounding log
        // noise but the envelope MUST be a JSON array (not the CLI verb's
        // {"devices": [...]} wrapper). The output also lands in the runtime
        // log, so it MUST terminate with exactly one newline so the next log
        // line starts on its own row.
        let line = list_input_devices_for_ui_json_line();
        assert!(line.ends_with('\n'), "expected trailing newline: {line:?}");
        let trimmed = line.trim();
        assert!(
            trimmed.starts_with('[') && trimmed.ends_with(']'),
            "expected a JSON array, got: {trimmed}"
        );
        let parsed: serde_json::Value = serde_json::from_str(trimmed).expect("valid JSON array");
        assert!(parsed.is_array(), "expected a JSON array: {parsed}");
    }

    #[test]
    fn append_extra_into_empty_list_uses_zero_index() {
        // If cpal enumerated nothing (WASAPI hiccup) but DirectSound sees a
        // mic, it still reaches the picker starting at synthetic index 0.
        let mut out: Vec<DeviceInfo> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        append_extra_named_devices(&["Only DirectSound Mic".to_owned()], &mut out, &mut seen);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].index, 0);
        assert_eq!(out[0].name, "Only DirectSound Mic");
    }

    // ----- Codex P2 (#663) regression tests live in the companion file ------
    //
    // The regression-test discipline scanner
    // (`src/tests/python/test_regression_test_discipline.py`) matches
    // NEW public symbols on their sibling `*_tests.rs` file, not on an
    // inline `mod tests` inside the changed production file. The four
    // regression tests for `EnumerationFlow` / `enumeration_flow` /
    // `should_merge_directsound_endpoints` therefore live in
    // `devices_tests.rs`; see the module-level doc-comment there for
    // the pre-fix / post-fix contract they pin.
}

// Companion test file discovered by the regression-test discipline
// scanner. See `devices_tests.rs` for the pinned invariants around
// `EnumerationFlow`, `enumeration_flow`, and
// `should_merge_directsound_endpoints`.
#[cfg(test)]
#[path = "devices_tests.rs"]
mod devices_regression_tests;
