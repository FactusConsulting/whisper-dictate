//! Parsing + display model for the worker's `--test-audio-device` JSON result.
//!
//! The worker prints a single JSON object describing whether the selected
//! microphone can be opened (and, when it can, which backend/rate/dtype it binds
//! on). That stdout can be preceded by ordinary log lines, so the parser carves
//! out the first `{` .. last `}` object span (reusing [`extract_json_object`])
//! and turns it into a small [`DeviceTestDisplay`] the Speech tab renders inline
//! as ✓ / ⚠ / ✗. Kept pure and free of egui so it unit-tests without a UI.

use super::worker_json::{extract_error_message, extract_json_object};
use serde::Deserialize;

/// The fields of the worker's `--test-audio-device` JSON the UI actually
/// renders. The contract also carries `device` (resolved name) and `dtype`
/// (int16/float32); serde ignores those extra keys here since the picker already
/// knows the device and the dtype isn't surfaced inline.
#[derive(Debug, Clone, Deserialize)]
struct DeviceTestResult {
    #[serde(default)]
    usable: bool,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    samplerate: Option<u32>,
    #[serde(default)]
    resampled: bool,
    #[serde(default)]
    reason: Option<String>,
}

/// How a parsed device-test result should be rendered next to the picker.
///
/// `Works` → ✓ green; `WorksWithCaveat` → ⚠ amber (e.g. opened via DirectSound,
/// or at a non-native rate that is resampled); `Cannot` → ✗ red with a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum DeviceTestOutcome {
    Works,
    WorksWithCaveat,
    Cannot,
}

/// The inline display model the Speech tab renders for a finished device test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct DeviceTestDisplay {
    pub(in crate::ui) outcome: DeviceTestOutcome,
    /// The endpoint token ("wasapi"/"directsound"/"mme"/"default") when usable.
    pub(in crate::ui) endpoint: Option<String>,
    pub(in crate::ui) samplerate: Option<u32>,
    pub(in crate::ui) resampled: bool,
    /// The short failure reason when not usable.
    pub(in crate::ui) reason: Option<String>,
}

/// Parse the worker's `--test-audio-device` stdout into a display model.
///
/// On a parseable result object this returns the [`DeviceTestDisplay`]. If the
/// worker emitted an `{"error": "..."}` object (or no object at all / malformed
/// JSON) this returns `Err` with a message the UI can show.
pub(in crate::ui) fn parse_device_test_json(stdout: &str) -> Result<DeviceTestDisplay, String> {
    let span = extract_json_object(stdout).ok_or_else(|| {
        extract_error_message(stdout)
            .unwrap_or_else(|| "no device-test result found in worker output".to_owned())
    })?;
    let result: DeviceTestResult =
        serde_json::from_str(span).map_err(|err| format!("could not parse test result: {err}"))?;
    Ok(display_from_result(result))
}

fn display_from_result(result: DeviceTestResult) -> DeviceTestDisplay {
    if !result.usable {
        return DeviceTestDisplay {
            outcome: DeviceTestOutcome::Cannot,
            endpoint: None,
            samplerate: None,
            resampled: false,
            reason: normalize(result.reason),
        };
    }
    // Usable, but worth a caveat when it isn't the clean WASAPI/native path: a
    // non-WASAPI endpoint (DirectSound/MME) or a resampled (non-16k) open both
    // mean "works, but not the ideal path" — surface that as ⚠ so the user knows.
    let endpoint = normalize(result.endpoint);
    let via_fallback_endpoint = matches!(endpoint.as_deref(), Some("directsound") | Some("mme"));
    let outcome = if result.resampled || via_fallback_endpoint {
        DeviceTestOutcome::WorksWithCaveat
    } else {
        DeviceTestOutcome::Works
    };
    DeviceTestDisplay {
        outcome,
        endpoint,
        samplerate: result.samplerate,
        resampled: result.resampled,
        reason: None,
    }
}

fn normalize(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Human label for an endpoint token, used in the ⚠ caveat line
/// ("Works via DirectSound"). Unknown tokens pass through capitalized.
pub(in crate::ui) fn endpoint_label(endpoint: &str) -> String {
    match endpoint.trim().to_ascii_lowercase().as_str() {
        "wasapi" => "WASAPI".to_owned(),
        "directsound" => "DirectSound".to_owned(),
        "mme" => "MME".to_owned(),
        "default" | "" => "default".to_owned(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => other.to_owned(),
            }
        }
    }
}

/// A plain, language-agnostic one-line summary of a device-test display, used
/// for the runtime log (the localized ✓/⚠/✗ rendering lives in the Speech tab).
pub(in crate::ui) fn device_test_log_detail(display: &DeviceTestDisplay) -> String {
    match display.outcome {
        DeviceTestOutcome::Works => {
            let endpoint = display.endpoint.as_deref().unwrap_or("default");
            match display.samplerate {
                Some(rate) => format!(
                    "works (endpoint={endpoint}, {})",
                    samplerate_khz_label(rate)
                ),
                None => format!("works (endpoint={endpoint})"),
            }
        }
        DeviceTestOutcome::WorksWithCaveat => {
            let endpoint = display.endpoint.as_deref().unwrap_or("default");
            let rate = display
                .samplerate
                .map(|r| format!(", {}", samplerate_khz_label(r)))
                .unwrap_or_default();
            let resampled = if display.resampled { ", resampled" } else { "" };
            format!("works with caveat (endpoint={endpoint}{rate}{resampled})")
        }
        DeviceTestOutcome::Cannot => {
            let reason = display.reason.as_deref().unwrap_or("unknown reason");
            format!("cannot be used ({reason})")
        }
    }
}

/// Format a sample rate (Hz) as a compact kHz label, e.g. 48000 → "48 kHz".
pub(in crate::ui) fn samplerate_khz_label(samplerate: u32) -> String {
    if samplerate.is_multiple_of(1000) {
        format!("{} kHz", samplerate / 1000)
    } else {
        format!("{:.1} kHz", samplerate as f32 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_wasapi_works() {
        let stdout = r#"whisper-dictate 1.9.5
{"device":"Yeti","usable":true,"endpoint":"wasapi","samplerate":16000,"dtype":"int16","resampled":false,"reason":null}"#;
        let d = parse_device_test_json(stdout).unwrap();
        assert_eq!(d.outcome, DeviceTestOutcome::Works);
        assert_eq!(d.endpoint.as_deref(), Some("wasapi"));
        assert_eq!(d.samplerate, Some(16000));
        assert!(!d.resampled);
        assert!(d.reason.is_none());
    }

    #[test]
    fn directsound_open_is_a_caveat() {
        let stdout = r#"{"device":"Yeti","usable":true,"endpoint":"directsound","samplerate":48000,"dtype":"int16","resampled":false}"#;
        let d = parse_device_test_json(stdout).unwrap();
        // Opened on a non-WASAPI endpoint → ⚠ "works with caveat".
        assert_eq!(d.outcome, DeviceTestOutcome::WorksWithCaveat);
        assert_eq!(d.endpoint.as_deref(), Some("directsound"));
    }

    #[test]
    fn resampled_wasapi_open_is_a_caveat() {
        let stdout = r#"{"device":"Webcam","usable":true,"endpoint":"wasapi","samplerate":48000,"dtype":"float32","resampled":true}"#;
        let d = parse_device_test_json(stdout).unwrap();
        assert_eq!(d.outcome, DeviceTestOutcome::WorksWithCaveat);
        assert!(d.resampled);
        assert_eq!(d.samplerate, Some(48000));
    }

    #[test]
    fn unusable_reports_cannot_with_reason() {
        let stdout = r#"{"device":"Dead Mic","usable":false,"endpoint":null,"samplerate":null,"dtype":null,"resampled":false,"reason":"could not open on any audio backend"}"#;
        let d = parse_device_test_json(stdout).unwrap();
        assert_eq!(d.outcome, DeviceTestOutcome::Cannot);
        assert_eq!(
            d.reason.as_deref(),
            Some("could not open on any audio backend")
        );
        assert!(d.endpoint.is_none());
    }

    #[test]
    fn device_not_found_is_cannot() {
        let stdout = r#"{"device":"Ghost","usable":false,"reason":"device not found"}"#;
        let d = parse_device_test_json(stdout).unwrap();
        assert_eq!(d.outcome, DeviceTestOutcome::Cannot);
        assert_eq!(d.reason.as_deref(), Some("device not found"));
    }

    #[test]
    fn tolerates_surrounding_log_noise() {
        let stdout = "[cap] probing\n[config] loaded\n\
            {\"device\":\"Mic\",\"usable\":true,\"endpoint\":\"default\",\"samplerate\":16000,\"dtype\":\"int16\",\"resampled\":false}\n\
            trailing log line\n";
        let d = parse_device_test_json(stdout).unwrap();
        assert_eq!(d.outcome, DeviceTestOutcome::Works);
        assert_eq!(d.endpoint.as_deref(), Some("default"));
    }

    #[test]
    fn reports_worker_error_object() {
        let stdout = "{\"error\": \"sounddevice unavailable: boom\"}";
        // An {"error": ...} object has no `usable` field → still parses as a
        // result with usable=false (serde default). It must NOT be mistaken for a
        // working device.
        let d = parse_device_test_json(stdout).unwrap();
        assert_eq!(d.outcome, DeviceTestOutcome::Cannot);
    }

    #[test]
    fn reports_missing_object_when_no_json_present() {
        let err = parse_device_test_json("just logs, no json\n").unwrap_err();
        assert!(err.contains("no device-test result found"), "{err}");
    }

    #[test]
    fn endpoint_label_maps_known_tokens() {
        assert_eq!(endpoint_label("wasapi"), "WASAPI");
        assert_eq!(endpoint_label("directsound"), "DirectSound");
        assert_eq!(endpoint_label("mme"), "MME");
        assert_eq!(endpoint_label("default"), "default");
        assert_eq!(endpoint_label("alsa"), "Alsa");
    }

    #[test]
    fn samplerate_khz_label_is_compact() {
        assert_eq!(samplerate_khz_label(48000), "48 kHz");
        assert_eq!(samplerate_khz_label(16000), "16 kHz");
        assert_eq!(samplerate_khz_label(44100), "44.1 kHz");
    }
}
