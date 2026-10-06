//! cpal host walking for the microphone picker: the default host first,
//! non-default hosts merged behind (de-duplicated by name), and
//! DirectSound-only endpoints appended last when the caller opts in.
use cpal::traits::{DeviceTrait, HostTrait};

use super::directsound::{append_extra_named_devices, directsound_capture_names};
use super::gate::{current_backend_is_rust, enumeration_flow, should_publish_device};
use super::DeviceInfo;

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
/// fail to open. The legacy sounddevice path can open DirectSound, so the
/// merge remains available to that compatibility caller.
pub(super) fn enumerate_all_hosts(include_directsound: bool) -> Vec<DeviceInfo> {
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
/// unconditionally" invariant can
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

/// Returns the first synthetic index to use for non-default-host devices:
/// `max(reported index) + 1` so synthetic indices never collide with the
/// default host's cpal-native indices even when the native range is sparse.
pub(crate) fn next_synthetic_from(devices: &[DeviceInfo]) -> usize {
    devices.iter().map(|d| d.index).max().map_or(0, |m| m + 1)
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
/// `input_devices()` itself fails — the picker never silently empties
/// the list when the backend is flaky, and the Settings UI relies on at least
/// the default mic appearing.
pub(crate) fn append_host_devices(
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
                        // a device `pick_config` cannot open.
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
            // Empty names collide with the UI's "(System default)"
            // sentinel, so we drop them just like select_input_devices does.
            continue;
        }
        // De-duplicate across hosts BY NAME. On Windows the default host
        // (WASAPI) already collapses host-API duplication, but cross-host
        // enumeration can re-introduce the same physical mic (e.g. ALSA
        // direct + Pulse default on Linux). The picker uses the same
        // bidirectional-substring rule for picker de-dup — we keep it simple
        // here with an exact case-insensitive name comparison, which covers
        // the same-physical-device case.
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
        // unit-testable without live audio hardware.
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

// The EnumerationFlow / should_merge_directsound_endpoints regression
// tests live in `devices_tests.rs` - the sibling file the regression-test
// discipline scanner matches on `src/rust/devices.rs`.
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
}
