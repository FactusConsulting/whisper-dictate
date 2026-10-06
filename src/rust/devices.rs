//!
//! The historical sounddevice path enumerated audio inputs via PortAudio and
//! collapsed the WASAPI/DirectSound/MME/WDM-KS duplication PortAudio exposes
//! on Windows down to a single entry per physical mic. cpal already enumerates
//! devices through the preferred host backend on each platform (WASAPI on
//! Windows, ALSA on Linux, CoreAudio on macOS), so this Rust port is the
//! equivalent: one entry per cpal input device, with non-default
//! hosts merged behind so PulseAudio/PipeWire/JACK setups don't hide USB mics.
//!
//! **Windows DirectSound parity.** cpal is WASAPI-only on Windows, but the
//! sounddevice picker deliberately surfaces DirectSound-exclusive inputs (a
//! freshly docked/hot-plugged USB mic can appear on DirectSound before WASAPI).
//! To reach parity — the prerequisite for defaulting the picker to this helper
//! `enumerate_all_hosts` also runs a native `DirectSoundCaptureEnumerateW`
//! pass on Windows and merges any DirectSound-only devices in by name (see
//! `append_extra_named_devices`). This is a no-op on other platforms.
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
//!     precedence required by the picker contract.
//!
//! The CLI subcommand `devices` (`handle_devices`) serialises the same list
//! as a JSON envelope for callers selecting the native devices backend.

//! Submodules: `lookup` (pure name matching), `gate` (capture-route
//! predicates), `enumeration` (cpal host walking), `directsound` (Windows
//! FFI + merge), and `request` (the hidden CLI envelope).

use serde::{Deserialize, Serialize};

use self::enumeration::enumerate_all_hosts;

/// One enumerated input device, shaped to match the existing picker JSON
/// contract so the UI and command-line callers need no translation.
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
    /// Matches the sounddevice JSON contract (`max_input_channels`). Entries
    /// with zero usable input configs are filtered out
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
/// wire shape used by the legacy `--list-audio-devices` command, so the
/// UI parser in [`crate::ui::parse_audio_devices_json`] stays authoritative.
///
/// Uses [`list_input_devices_with_directsound`] so a freshly docked/hot-plugged
/// mic visible only on Windows DirectSound still shows up in the picker (the
/// UI-side sounddevice equivalent). Empty result serialises as `"[]"`.
///
/// Exposed as `pub` so `ui::tasks::run_list_audio_devices` can call it in a
/// background thread without spawning a subprocess. Result is a single JSON
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

/// Find a device by name using the picker contract's precedence:
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

// Name matching, capture-route gating, cpal enumeration, the Windows
// DirectSound merge, and the hidden CLI envelope each live in their own
// submodule so no single file carries the whole pipeline.
mod directsound;
mod enumeration;
mod gate;
mod lookup;
mod request;

pub use self::directsound::directsound_capture_names_public;
pub use self::lookup::find_in;
pub use self::request::handle_devices;

pub(crate) use self::lookup::name_matches;

#[cfg(test)]
mod tests {
    use super::list_input_devices_for_ui_json_line;

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
}

// Companion test file discovered by the regression-test discipline
// scanner. See `devices_tests.rs` for the pinned invariants around
// `EnumerationFlow`, `enumeration_flow`, and
// `should_merge_directsound_endpoints`.
#[cfg(test)]
#[path = "devices_tests.rs"]
mod devices_regression_tests;
