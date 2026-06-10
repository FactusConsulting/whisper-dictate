//! Parsing for the worker's `--list-audio-devices` JSON output.
//!
//! The worker prints a JSON array of input devices on stdout, but that stdout
//! can be preceded by ordinary log lines (version banner, config notes). The
//! parser is kept pure and small so it is easy to unit-test: it carves out the
//! first `[` .. last `]` span, parses it, and turns it into the combo labels the
//! Speech tab shows.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct AudioDevice {
    name: String,
    #[serde(default)]
    default: bool,
}

/// A parsed device-list entry: the value persisted to `audio_device` (the raw
/// device name) plus the human label shown in the combo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct DeviceOption {
    pub(in crate::ui) value: String,
    pub(in crate::ui) label: String,
}

/// Extract a JSON array span (`[` .. last `]`) from a stdout blob that may carry
/// surrounding log noise. Log lines often contain brackets (e.g. `[config]`),
/// so the opening bracket is the first `[` whose next non-whitespace character
/// actually starts a JSON value (`{ [ " - digit t f n`) or closes an empty
/// array (`]`); anything else (a log tag) is skipped. Returns `None` when no
/// such array start pairs with a trailing `]`.
fn extract_json_array(stdout: &str) -> Option<&str> {
    let end = stdout.rfind(']')?;
    let mut search = 0;
    while let Some(rel) = stdout[search..=end].find('[') {
        let start = search + rel;
        let next = stdout[start + 1..=end]
            .bytes()
            .find(|b| !b.is_ascii_whitespace());
        let looks_like_array = match next {
            None => false,
            Some(b) => {
                matches!(b, b'{' | b'[' | b'"' | b']' | b'-' | b't' | b'f' | b'n')
                    || b.is_ascii_digit()
            }
        };
        if looks_like_array {
            return Some(&stdout[start..=end]);
        }
        if start >= end {
            break;
        }
        search = start + 1;
    }
    None
}

/// Parse the worker's `--list-audio-devices` stdout into combo options.
///
/// On success returns the device list (the caller prepends a "(System default)"
/// entry). The worker reports errors as a `{"error": "..."}` object, not an
/// array; that surfaces here as an `Err` carrying the message. Malformed or
/// array-free output also yields an `Err` so the UI can report it.
pub(in crate::ui) fn parse_audio_devices_json(stdout: &str) -> Result<Vec<DeviceOption>, String> {
    let span = extract_json_array(stdout).ok_or_else(|| {
        if let Some(message) = extract_error_message(stdout) {
            message
        } else {
            "no device list found in worker output".to_owned()
        }
    })?;
    let devices: Vec<AudioDevice> =
        serde_json::from_str(span).map_err(|err| format!("could not parse device list: {err}"))?;
    Ok(devices.into_iter().map(device_option).collect())
}

/// Pull the `error` field out of a `{"error": "..."}` object, if present, so the
/// no-array path can report the worker's own message instead of a generic one.
fn extract_error_message(stdout: &str) -> Option<String> {
    let start = stdout.find('{')?;
    let end = stdout.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&stdout[start..=end]).ok()?;
    value
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn device_option(device: AudioDevice) -> DeviceOption {
    let mut label = device.name.clone();
    if device.default {
        label.push_str(" (default)");
    }
    DeviceOption {
        value: device.name,
        label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_json_array() {
        let stdout = r#"[{"index":0,"name":"Yeti","max_input_channels":2,"default":true},
            {"index":1,"name":"Webcam Mic","max_input_channels":1,"default":false}]"#;
        let options = parse_audio_devices_json(stdout).unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].value, "Yeti");
        assert_eq!(options[0].label, "Yeti (default)");
        assert_eq!(options[1].value, "Webcam Mic");
        assert_eq!(options[1].label, "Webcam Mic");
    }

    #[test]
    fn tolerates_surrounding_log_noise() {
        let stdout = "whisper-dictate 1.8.4\n[config] loaded\n\
            [{\"index\":3,\"name\":\"Focusrite\",\"max_input_channels\":2,\"default\":false}]\n\
            trailing log line\n";
        let options = parse_audio_devices_json(stdout).unwrap();
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].value, "Focusrite");
    }

    #[test]
    fn skips_bracketed_log_tags_before_the_array() {
        // Log tags like [cap]/[config] contain brackets that must NOT be mistaken
        // for the JSON array start.
        let stdout = "[cap] probing\n[config] loaded ok\n\
            [{\"index\":0,\"name\":\"Mic\",\"max_input_channels\":1,\"default\":true}]";
        let options = parse_audio_devices_json(stdout).unwrap();
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].value, "Mic");
    }

    #[test]
    fn empty_array_yields_no_options() {
        let options = parse_audio_devices_json("noise\n[]\nmore").unwrap();
        assert!(options.is_empty());
    }

    #[test]
    fn reports_worker_error_object() {
        let stdout = "whisper-dictate 1.8.4\n{\"error\": \"sounddevice unavailable: boom\"}\n";
        let err = parse_audio_devices_json(stdout).unwrap_err();
        assert!(err.contains("sounddevice unavailable: boom"), "{err}");
    }

    #[test]
    fn reports_missing_array_when_no_json_present() {
        let err = parse_audio_devices_json("just some logs, no json here\n").unwrap_err();
        assert!(err.contains("no device list found"), "{err}");
    }

    #[test]
    fn reports_malformed_array() {
        let err = parse_audio_devices_json("[ this is not json ]").unwrap_err();
        assert!(err.contains("could not parse device list"), "{err}");
    }
}
