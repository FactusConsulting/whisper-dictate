//! Rust port of `src/python/whisper_dictate/vp_devices.py` — input-device
//! enumeration for the microphone picker.
//!
//! The Python module enumerates audio inputs via PortAudio (sounddevice) and
//! collapses the WASAPI/DirectSound/MME/WDM-KS duplication PortAudio exposes
//! on Windows down to a single entry per physical mic. cpal already enumerates
//! devices through the preferred host backend on each platform (WASAPI on
//! Windows, ALSA on Linux, CoreAudio on macOS), so this Rust port is the
//! cheap-and-clean equivalent: one entry per cpal input device, no host-API
//! collapsing needed because cpal has already done it.
//!
//! The module is gated behind the `audio-in-rust` cargo feature (cpal is the
//! only heavy native dep this pulls in, and the audio feature has the same
//! libasound requirement on Linux, so it makes sense to share the gate).
//!
//! Public API mirrors the shape the Settings UI / picker expects:
//!   * [`list_input_devices`] → `Vec<DeviceInfo>` (default flag set on the
//!     host's default input).
//!   * [`default_input_device`] → `Option<DeviceInfo>` for the platform
//!     default.
//!   * [`find_device_by_name`] → exact + case-insensitive substring match,
//!     same precedence the Python resolver uses.
//!
//! The CLI subcommand `devices` (`handle_devices`) serialises the same list
//! as a JSON envelope so `vp_devices.py` can shell out to it when
//! `VOICEPI_DEVICES_BACKEND=rust` is set.

use std::io::{self, Read};

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
    /// Position in the enumeration order (parallels Python's
    /// `sd.query_devices()` index — but compacted across filtered entries).
    pub index: usize,
    /// Human-readable device name (cpal's `Display` impl on every backend).
    pub name: String,
    /// Maximum input channel count the device reports. Entries with zero
    /// input channels are filtered out upstream, so any value here is ≥ 1.
    pub max_input_channels: u16,
    /// `(min_hz, max_hz)` from cpal's supported-input-configs union.
    /// `(0, 0)` when the device exposes no input configs (extremely rare;
    /// defensive against backend quirks).
    pub sample_rates: (u32, u32),
    /// True when this entry matches the host's default input device by name.
    /// We compare by NAME (not by object identity) because cpal's `Device`
    /// values returned from `default_input_device()` and `input_devices()` are
    /// distinct handles even when they refer to the same physical device.
    pub default: bool,
}

/// Enumerate every input device the platform exposes, in the host iterator's
/// natural order. Devices with zero input channels or blank names are
/// filtered out so a caller can show the list verbatim without re-filtering.
pub fn list_input_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .map(|d| d.to_string())
        .unwrap_or_default();
    enumerate_with_host(&host, &default_name)
}

/// The host's default input device, if any. Returns the same `DeviceInfo`
/// shape so the UI can render it identically to the picker entries.
pub fn default_input_device() -> Option<DeviceInfo> {
    let host = cpal::default_host();
    let default = host.default_input_device()?;
    let name = default.to_string();
    Some(build_device_info(0, &default, &name, true))
}

/// Find a device by name. Precedence matches the Python resolver:
///   1. case-insensitive EXACT name match wins,
///   2. otherwise case-insensitive SUBSTRING match (bidirectional — saved
///      name in device name, or device name in saved value — so an
///      MME-truncated saved value still maps to its full WASAPI name).
///
/// Returns `None` if no device matches.
pub fn find_device_by_name(query: &str) -> Option<DeviceInfo> {
    let devices = list_input_devices();
    find_in(&devices, query).cloned()
}

// ----- pure helpers (unit-testable without a real cpal host) ------------------

/// Pure name lookup. Exposed so the test suite can exercise it against a
/// hand-rolled device list without depending on a live audio backend.
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
    //    vp_devices._name_matches: either side may be the prefix.
    devices.iter().find(|d| {
        let lower = d.name.to_lowercase();
        !lower.is_empty() && (lower.contains(&folded) || folded.contains(&lower))
    })
}

fn enumerate_with_host(host: &cpal::Host, default_name: &str) -> Vec<DeviceInfo> {
    let devices = match host.input_devices() {
        Ok(iter) => iter.collect::<Vec<_>>(),
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::with_capacity(devices.len());
    let mut next_index = 0usize;
    for device in devices {
        let name = device.to_string();
        if name.trim().is_empty() {
            // Empty names collide with the Python UI's "(System default)"
            // sentinel, so we drop them just like select_input_devices does.
            continue;
        }
        let info = build_device_info(next_index, &device, &name, name == default_name);
        if info.max_input_channels == 0 {
            // No-input-channel entries (some virtual / loopback devices)
            // never belong in the input picker.
            continue;
        }
        out.push(info);
        next_index += 1;
    }
    out
}

fn build_device_info(index: usize, device: &cpal::Device, name: &str, default: bool) -> DeviceInfo {
    let (channels, sample_rates) = match device.default_input_config() {
        Ok(cfg) => {
            // cpal 0.18 type-aliased `SampleRate` to a plain `u32` (no
            // tuple-struct .0 field) — see audio/capture.rs for the same
            // pattern. `sample_rate()` and `{min,max}_sample_rate()` return
            // the rate directly.
            let cfg_rate: u32 = cfg.sample_rate();
            let (mut lo, mut hi) = (cfg_rate, cfg_rate);
            if let Ok(supported) = device.supported_input_configs() {
                for sc in supported {
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
            (cfg.channels(), (lo, hi))
        }
        Err(_) => (0, (0u32, 0u32)),
    };
    DeviceInfo {
        index,
        name: name.to_owned(),
        max_input_channels: channels,
        sample_rates,
        default,
    }
}

// ----- CLI handler ------------------------------------------------------------

/// JSON request envelope for the hidden `devices` sub-command. Mirrors the
/// shape `handle_health` uses (action-tagged enum) so the Python shell-out
/// can pick the operation it wants without parsing multiple positional args.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum DevicesRequest {
    /// List every input device.
    List,
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

/// Handler for the hidden `devices` sub-command. Reads a JSON request from
/// stdin and writes a JSON response on stdout.
///
/// Accepts an empty / missing stdin body as a shorthand for
/// `{"action":"list"}` so callers that just want the list can pipe nothing in.
pub fn handle_devices() -> Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let trimmed = raw.trim();
    let request: DevicesRequest = if trimmed.is_empty() {
        DevicesRequest::List
    } else {
        serde_json::from_str(trimmed)?
    };
    match request {
        DevicesRequest::List => {
            let resp = ListResponse {
                devices: list_input_devices(),
            };
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
        // hit must bind to the first entry, not the longer sibling.
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
        assert!(matches!(parsed, DevicesRequest::List));
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
}
