//! Native cpal-based device probe for the `devices test <NAME>` CLI verb and
//! the UI's "Test Device" button (step 1 of the `vp_device_test.py` retirement
//! — issue #348).
//!
//! Emits the SAME JSON envelope the Python worker's `--test-audio-device`
//! query mode did, so the UI parser in [`crate::ui::device_test`] keeps working
//! unchanged. The wire fields are:
//!
//! ```json
//! {
//!   "device": "<resolved name>",
//!   "usable": true|false,
//!   "endpoint": "wasapi"|"directsound"|"mme"|"default"|null,
//!   "samplerate": <int>|null,
//!   "dtype": "int16"|"float32"|"int32"|null,
//!   "resampled": true|false,
//!   "reason": "<short failure reason>"|null
//! }
//! ```
//!
//! ## Endpoint token
//!
//! cpal always opens through the platform's default host — WASAPI on Windows,
//! ALSA on Linux, CoreAudio on macOS. So the probe reports `"wasapi"` on
//! Windows and `"default"` everywhere else (the same token
//! [`crate::ui::device_test::endpoint_label`] renders as "default"). The
//! DirectSound / MME endpoints the Python probe returned are inaccessible via
//! cpal; they'll re-appear when the Python code is deleted in step 2 iff we
//! add a native fallback, but until then this Rust probe mirrors what actual
//! Rust capture would open.
//!
//! ## Resampled flag
//!
//! Rust live capture (see [`crate::audio::capture::pick_config`]) always picks
//! the device's native sample rate and resamples to 16 kHz downstream, so
//! `resampled` here reflects the same truth: `true` whenever the negotiated
//! rate is not 16 kHz. That matches what the UI's inline caveat would show
//! for the actual capture path.
//!
//! ## No audio is captured
//!
//! The probe builds the input stream, calls `play()` to prove the device
//! opens end to end (some backends only surface a "device busy" error at
//! stream-start time, not at build-time), then immediately drops the stream.
//! No callback data is retained — the probe is purely a dry run.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use serde::Serialize;

use crate::audio::capture::{resolve_device_index, DeviceLookup};

/// One probe outcome — the exact wire shape the UI's `--test-audio-device`
/// parser expects. `endpoint` / `samplerate` / `dtype` are only populated on
/// success; `reason` only on failure. `dtype` mirrors the Python labelling:
/// float32 for cpal's F32 path, int16 for I16, int32 for I32 (the third case
/// the Python probe never produced — cpal is the only path that opens I32
/// natively; the label still renders sensibly in the log-detail).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceProbeResult {
    /// Resolved device name (or "System default" when the caller passed "").
    pub device: String,
    pub usable: bool,
    /// Endpoint token — see the module docs for the platform mapping.
    pub endpoint: Option<String>,
    pub samplerate: Option<u32>,
    /// Sample-format label — `"int16"` / `"float32"` / `"int32"`, matching
    /// what the Python probe used.
    pub dtype: Option<String>,
    /// True when the negotiated rate isn't 16 kHz (i.e. live capture will
    /// resample). Same semantics as the Python probe.
    pub resampled: bool,
    pub reason: Option<String>,
}

impl DeviceProbeResult {
    /// Serialise as the single-line JSON envelope the CLI writes to stdout
    /// and the UI parser reads. `serde_json` is already in the dep graph via
    /// several other crates; using it here keeps the field-order + null
    /// handling consistent with every other JSON envelope this binary emits.
    pub fn to_json_line(&self) -> String {
        // Serialising via a struct with `#[derive(Serialize)]` on
        // `Option` fields already emits `null` for `None`, matching the
        // Python envelope 1:1.
        serde_json::to_string(self)
            .unwrap_or_else(|_| String::from(r#"{"usable":false,"reason":"serialize failed"}"#))
    }

    fn ok(device: String, endpoint: &str, samplerate: u32, dtype: &str, resampled: bool) -> Self {
        Self {
            device,
            usable: true,
            endpoint: Some(endpoint.to_owned()),
            samplerate: Some(samplerate),
            dtype: Some(dtype.to_owned()),
            resampled,
            reason: None,
        }
    }

    fn fail(device: String, reason: impl Into<String>) -> Self {
        Self {
            device,
            usable: false,
            endpoint: None,
            samplerate: None,
            dtype: None,
            resampled: false,
            reason: Some(reason.into()),
        }
    }
}

/// Endpoint token cpal exposes on this platform. cpal has a single default
/// host per OS (WASAPI on Windows, ALSA on Linux, CoreAudio on macOS), so
/// the mapping is a compile-time constant.
pub(crate) fn default_endpoint_token() -> &'static str {
    if cfg!(windows) {
        "wasapi"
    } else {
        "default"
    }
}

/// Label the cpal `SampleFormat` with the Python-side dtype token so the
/// UI's log-detail line and the JSON envelope keep the shape they had when
/// the probe was Python.
pub(crate) fn dtype_label(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::F32 => "float32",
        SampleFormat::I16 => "int16",
        SampleFormat::I32 => "int32",
        _ => "unknown",
    }
}

/// Whether a negotiated `rate` should be flagged as resampled. Live capture
/// downsamples every non-16k stream to 16 kHz (see the resampler stage in
/// `audio::mod`), so `true` iff `rate != 16000`.
pub(crate) fn is_resampled(rate: u32) -> bool {
    rate != 16_000
}

/// Dry-run open the input device selected by `requested` and return the wire
/// envelope. Empty `requested` picks the host's default input (same semantics
/// as `dictate-mic --device ""`).
///
/// Never panics: enumeration failures, device-not-found, unsupported configs
/// and start-time errors all funnel into a `usable: false` result with a
/// short `reason`. Successful open picks the F32-first / I16 / I32 config at
/// the device's native rate — mirroring [`crate::audio::capture::pick_config`]
/// so the report reflects what live capture would actually negotiate.
pub fn probe_device(requested: &str) -> DeviceProbeResult {
    let trimmed = requested.trim();
    let requested_label = if trimmed.is_empty() {
        "System default".to_owned()
    } else {
        trimmed.to_owned()
    };

    let host = cpal::default_host();

    let device = if trimmed.is_empty() {
        match host.default_input_device() {
            Some(d) => d,
            None => {
                return DeviceProbeResult::fail(
                    requested_label,
                    "no default input device available",
                );
            }
        }
    } else {
        let devices: Vec<cpal::Device> = match host.input_devices() {
            Ok(iter) => iter.collect(),
            Err(err) => {
                return DeviceProbeResult::fail(
                    requested_label,
                    format!("enumerate input devices: {err}"),
                );
            }
        };
        let names: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
        match resolve_device_index(&names, trimmed) {
            DeviceLookup::Matched(idx) => devices
                .into_iter()
                .nth(idx)
                .expect("resolve_device_index returned an in-range index"),
            DeviceLookup::IndexOutOfRange { wanted, available } => {
                return DeviceProbeResult::fail(
                    requested_label,
                    format!(
                        "input device index {wanted} out of range (have {available} input device(s))"
                    ),
                );
            }
            DeviceLookup::NotFound => {
                // Match the Python envelope for a name that didn't resolve so
                // the UI shows the same short "device not found" reason.
                return DeviceProbeResult::fail(requested_label, "device not found");
            }
        }
    };

    // cpal 0.18 removed `Device::name()` in favour of the `Display` impl (see
    // the same use in `capture.rs::pick_device`), so use `to_string()` here.
    let resolved_label = device.to_string();
    let resolved_label = if resolved_label.trim().is_empty() {
        requested_label.clone()
    } else {
        resolved_label
    };

    let supported = match pick_config(&device) {
        Ok(cfg) => cfg,
        Err(err) => {
            return DeviceProbeResult::fail(
                resolved_label,
                format!("could not open on any audio backend: {err}"),
            );
        }
    };
    let sample_format = supported.sample_format();
    // cpal 0.18 type-aliased `SampleRate` to a plain `u32`, so the call
    // returns the rate directly (no `.0` tuple accessor) — same shape the
    // capture module uses.
    let sample_rate: u32 = supported.sample_rate();
    let config: cpal::StreamConfig = supported.into();

    // Build + play + drop, so we exercise the same code path a real capture
    // open would — some backends (notably WASAPI) only fail at play() time
    // for an in-use device, not at build_input_stream(). cpal 0.18 takes
    // `StreamConfig` by value and adds an explicit `timeout` arg (see
    // `capture::build_input_stream`); `None` matches "block until open".
    // cpal 0.18 unified build/stream errors under `cpal::Error`; the old
    // `BuildStreamError` alias is gone (see `capture.rs::build_input_stream`
    // for the same shape).
    let build_result: Result<cpal::Stream, cpal::Error> = match sample_format {
        SampleFormat::F32 => {
            device.build_input_stream::<f32, _, _>(config, |_data, _info| {}, |_err| {}, None)
        }
        SampleFormat::I16 => {
            device.build_input_stream::<i16, _, _>(config, |_data, _info| {}, |_err| {}, None)
        }
        SampleFormat::I32 => {
            device.build_input_stream::<i32, _, _>(config, |_data, _info| {}, |_err| {}, None)
        }
        other => {
            return DeviceProbeResult::fail(
                resolved_label,
                format!("unsupported sample format negotiated: {other:?}"),
            );
        }
    };
    let stream = match build_result {
        Ok(s) => s,
        Err(err) => {
            return DeviceProbeResult::fail(
                resolved_label,
                format!("could not open on any audio backend: {err}"),
            );
        }
    };
    if let Err(err) = stream.play() {
        // Failing to play means the OS wouldn't hand us the device even
        // though we could build a config for it (in-use / permission). Same
        // "device unusable" verdict as Python.
        drop(stream);
        return DeviceProbeResult::fail(
            resolved_label,
            format!("could not open on any audio backend: {err}"),
        );
    }
    // Drop the stream immediately — no audio is captured. This releases the
    // device before returning so a subsequent probe (or real capture) can
    // reopen it without racing against a still-live stream.
    drop(stream);

    DeviceProbeResult::ok(
        resolved_label,
        default_endpoint_token(),
        sample_rate,
        dtype_label(sample_format),
        is_resampled(sample_rate),
    )
}

/// Pick the best supported input config for `device`. Identical priority to
/// [`crate::audio::capture::pick_config`] — F32 > I16 > I32 at the device's
/// native rate — so the probe reports what live capture will actually
/// negotiate. Kept in this module (rather than reusing capture's private
/// helper) so the probe is a self-contained unit and can be unit-tested.
fn pick_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, anyhow::Error> {
    let mut best_f32: Option<cpal::SupportedStreamConfigRange> = None;
    let mut best_i16: Option<cpal::SupportedStreamConfigRange> = None;
    let mut best_i32: Option<cpal::SupportedStreamConfigRange> = None;

    let supported = device
        .supported_input_configs()
        .map_err(|err| anyhow::anyhow!("supported_input_configs: {err}"))?;
    for cfg in supported {
        match cfg.sample_format() {
            SampleFormat::F32 => best_f32 = Some(cfg),
            SampleFormat::I16 => best_i16 = Some(cfg),
            SampleFormat::I32 => best_i32 = Some(cfg),
            _ => {}
        }
    }
    let picked = best_f32
        .or(best_i16)
        .or(best_i32)
        .ok_or_else(|| anyhow::anyhow!("no F32/I16/I32 input config supported"))?;
    Ok(picked.with_max_sample_rate())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dtype_label_matches_python_wire_tokens() {
        // The Python probe emitted "int16" / "float32" — the UI's log-detail
        // line and JSON envelope must keep those exact tokens so a downstream
        // consumer that greps for them still works after the migration.
        assert_eq!(dtype_label(SampleFormat::F32), "float32");
        assert_eq!(dtype_label(SampleFormat::I16), "int16");
        assert_eq!(dtype_label(SampleFormat::I32), "int32");
    }

    #[test]
    fn endpoint_token_is_platform_specific() {
        // cpal is WASAPI-only on Windows, ALSA on Linux, CoreAudio on macOS
        // — one default host per OS, so the mapping is a compile-time
        // constant. This pins the token so a future refactor can't drop it.
        let token = default_endpoint_token();
        if cfg!(windows) {
            assert_eq!(token, "wasapi");
        } else {
            assert_eq!(token, "default");
        }
    }

    #[test]
    fn is_resampled_true_only_when_rate_not_16k() {
        // Live capture always downsamples to 16 kHz, so the `resampled`
        // flag is true iff the negotiated rate isn't already 16 kHz. Pin
        // both the boundary (16000 is the ONLY false case) and a few
        // typical rates so a refactor can't silently move the boundary.
        assert!(!is_resampled(16_000));
        assert!(is_resampled(8_000));
        assert!(is_resampled(44_100));
        assert!(is_resampled(48_000));
        assert!(is_resampled(96_000));
    }

    #[test]
    fn envelope_success_shape_matches_ui_parser_contract() {
        // The UI's parse_device_test_json requires `usable`, `endpoint`,
        // `samplerate`, `dtype`, `resampled`, `reason` fields. Serialise a
        // canonical success result and cross-check every field lands with
        // the expected null / value shape.
        let result = DeviceProbeResult::ok("Yeti".to_owned(), "wasapi", 16_000, "int16", false);
        let json: serde_json::Value =
            serde_json::from_str(&result.to_json_line()).expect("valid JSON");
        assert_eq!(json["device"], "Yeti");
        assert_eq!(json["usable"], true);
        assert_eq!(json["endpoint"], "wasapi");
        assert_eq!(json["samplerate"], 16_000);
        assert_eq!(json["dtype"], "int16");
        assert_eq!(json["resampled"], false);
        assert!(json["reason"].is_null());
    }

    #[test]
    fn envelope_failure_shape_matches_ui_parser_contract() {
        // On failure the reason MUST be populated and every open-only field
        // MUST serialise as JSON null so the UI renders a red ✗ with the
        // short reason (and never a spurious samplerate/endpoint pill).
        let result = DeviceProbeResult::fail("Ghost".to_owned(), "device not found");
        let json: serde_json::Value =
            serde_json::from_str(&result.to_json_line()).expect("valid JSON");
        assert_eq!(json["device"], "Ghost");
        assert_eq!(json["usable"], false);
        assert!(json["endpoint"].is_null());
        assert!(json["samplerate"].is_null());
        assert!(json["dtype"].is_null());
        assert_eq!(json["resampled"], false);
        assert_eq!(json["reason"], "device not found");
    }

    #[test]
    fn envelope_json_is_a_single_line() {
        // The UI parser tolerates surrounding log noise but every worker
        // envelope has always been a single JSON object on its own line. A
        // multi-line pretty-print would still parse, but would surprise any
        // downstream tool that greps by line — so pin this.
        let json = DeviceProbeResult::ok("Mic".to_owned(), "default", 48_000, "float32", true)
            .to_json_line();
        assert!(!json.contains('\n'), "envelope must be one line: {json}");
    }

    #[test]
    fn empty_selector_probe_never_panics_and_reports_when_no_default() {
        // Empty selector = system default. On a headless dev box with no
        // default input, this must NOT panic — it must produce a
        // well-formed unusable envelope so the CLI still emits valid JSON.
        // Whichever way the host answers, the parser MUST cope.
        let r = probe_device("");
        let json: serde_json::Value = serde_json::from_str(&r.to_json_line()).expect("valid JSON");
        // Either the box has a default input (usable=true) OR it doesn't
        // (usable=false with a reason). No third state.
        assert!(json["usable"].is_boolean());
        if !r.usable {
            assert!(r.reason.is_some(), "unusable result must carry a reason");
        }
    }

    #[test]
    fn missing_named_device_reports_not_found_without_panicking() {
        // A name that cannot resolve on any host MUST report the short
        // "device not found" reason (the same string the Python probe used)
        // — that's what the UI's inline ✗ + reason renders.
        let r = probe_device("__whisper_dictate_definitely_missing_device__");
        assert!(!r.usable);
        assert_eq!(r.reason.as_deref(), Some("device not found"));
        assert!(r.endpoint.is_none());
        assert!(r.samplerate.is_none());
        assert!(r.dtype.is_none());
        assert!(!r.resampled);
    }
}
