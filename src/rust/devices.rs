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
//! The module is gated behind the `audio-in-rust` cargo feature (cpal is the
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
    enumerate_all_hosts()
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
/// When `VOICEPI_AUDIO_BACKEND=rust` the Rust capture path calls
/// `cpal::default_host()` and cannot open devices from other hosts. In that
/// configuration only the default host's devices are returned so the picker
/// never advertises a mic that capture would fail to open.
fn enumerate_all_hosts() -> Vec<DeviceInfo> {
    let default_host = cpal::default_host();
    let default_host_id = default_host.id();
    let default_input_index = default_input_index(&default_host);

    let mut out: Vec<DeviceInfo> = Vec::new();
    let mut seen_names: Vec<String> = Vec::new();
    append_host_devices(
        &default_host,
        /*default_input_index=*/ default_input_index,
        /*is_default_host=*/ true,
        /*next_synthetic_index=*/ &mut 0,
        &mut out,
        &mut seen_names,
    );

    // When Rust capture is active it uses cpal::default_host() and cannot
    // resolve non-default-host devices, so skip them entirely.
    let rust_capture = std::env::var("VOICEPI_AUDIO_BACKEND")
        .ok()
        .map(|v| v.trim().eq_ignore_ascii_case("rust"))
        .unwrap_or(false);
    if rust_capture {
        return out;
    }

    // Non-default-host devices are useful for name-based selection so a saved
    // mic exposed only via JACK/ASIO still shows up in the picker.
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
        let mut next_synthetic = next_synthetic_from(&out);
        append_host_devices(
            &host,
            /*default_input_index=*/ None,
            /*is_default_host=*/ false,
            &mut next_synthetic,
            &mut out,
            &mut seen_names,
        );
    }

    out
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
/// `input_devices()` itself fails — the Python picker never silently empties
/// the list when the backend is flaky, and the Settings UI relies on at least
/// the default mic appearing.
fn append_host_devices(
    host: &cpal::Host,
    default_input_index: Option<usize>,
    is_default_host: bool,
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
                        if info.max_input_channels > 0 {
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
        if info.max_input_channels == 0 {
            // No usable input configs at all (neither default_input_config nor
            // supported_input_configs reported channels). Skip — the picker
            // can't open it.
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
pub(crate) fn resolve_devices_request(
    stdin_is_tty: bool,
    stdin_body: Option<&str>,
) -> Result<DevicesRequest> {
    if stdin_is_tty {
        return Ok(DevicesRequest::List);
    }
    let trimmed = stdin_body.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(DevicesRequest::List);
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

    // ----- resolve_devices_request: TTY vs pipe dispatch (Codex PR #564 P2) --

    #[test]
    fn resolve_defaults_to_list_when_stdin_is_a_tty() {
        // An interactive `whisper-dictate devices` in PowerShell has no
        // piped body — we skip the blocking read and default to the list so
        // the user sees output instead of the process hanging on stdin.
        let request = resolve_devices_request(true, None).unwrap();
        assert!(matches!(request, DevicesRequest::List));
    }

    #[test]
    fn resolve_defaults_to_list_when_stdin_is_a_tty_even_with_body() {
        // Defensive: if a caller ever passes a body while claiming TTY,
        // the TTY branch still wins (matches the doc contract — TTY means
        // interactive convenience regardless of the body).
        let request =
            resolve_devices_request(true, Some(r#"{"action":"find","query":"x"}"#)).unwrap();
        assert!(matches!(request, DevicesRequest::List));
    }

    #[test]
    fn resolve_defaults_to_list_when_piped_stdin_is_empty() {
        // The Python shell-out sometimes pipes nothing and expects a list —
        // this is the documented shorthand for `{"action":"list"}`.
        assert!(matches!(
            resolve_devices_request(false, Some("")).unwrap(),
            DevicesRequest::List
        ));
        assert!(matches!(
            resolve_devices_request(false, Some("   \n  ")).unwrap(),
            DevicesRequest::List
        ));
        assert!(matches!(
            resolve_devices_request(false, None).unwrap(),
            DevicesRequest::List
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
}
