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
//! The probe reports the label of whichever cpal host actually opened the
//! device. On Windows that is `"wasapi"` today (cpal 0.18 has no
//! DirectSound host — the picker's DirectSound-only names are unopenable);
//! on Linux it is one of `"alsa"` / `"pulseaudio"` / `"pipewire"` / `"jack"`
//! depending on which cpal features were compiled in and which host
//! actually surfaced the device; on macOS it is `"coreaudio"`. Empty
//! selector always opens on the platform default host, so its label is the
//! default host's label.
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

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::SampleFormat;
use serde::Serialize;

use crate::audio::hosts::{resolve_input, ResolvedInput};

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

/// Endpoint token cpal exposes on this platform, for the DEFAULT host.
/// Used when the probe resolves the system default device (empty
/// selector); a named device always reports the label of the host that
/// actually opened it, which may not be the default (see
/// [`endpoint_token_for_host`]).
pub(crate) fn default_endpoint_token() -> &'static str {
    if cfg!(windows) {
        "wasapi"
    } else {
        "default"
    }
}

/// Map cpal's host label (`"WASAPI"`, `"ALSA"`, `"PulseAudio"`,
/// `"PipeWire"`, `"JACK"`, `"ASIO"`, `"CoreAudio"`, …) to the lowercased
/// endpoint token the UI parser + JSON envelope have always emitted. This
/// is the point where the multi-host resolver rejoins the existing
/// wire-format contract; keeping the mapping in one place means adding a
/// host later is a one-line arm change.
pub(crate) fn endpoint_token_for_host(host_label: &str) -> String {
    // The UI's `endpoint_label` renders our lowercase tokens verbatim, so
    // matching sounddevice's historic vocabulary keeps the picker column
    // stable. Unknown labels fall through as lowercased-as-is so a new
    // cpal host still surfaces something inspectable in the log-detail.
    match host_label {
        "WASAPI" => "wasapi".to_owned(),
        "ALSA" => "alsa".to_owned(),
        "PulseAudio" => "pulseaudio".to_owned(),
        "PipeWire" => "pipewire".to_owned(),
        "JACK" => "jack".to_owned(),
        "ASIO" => "asio".to_owned(),
        "CoreAudio" => "coreaudio".to_owned(),
        other => other.to_lowercase(),
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

/// Translate a [`crate::audio::hosts::resolve_input`] error message into
/// the short probe `reason` string the UI parser + JSON envelope render.
///
/// * Preserves the historic short wording for the two most common
///   failure modes (`no default input device available` and `device not
///   found`) so the UI's ✗ + reason line reads the same as when the
///   probe was Python.
/// * For the "not found" case ALSO appends the Windows DirectSound
///   `pick the WASAPI variant` remediation when the caller-provided
///   `directsound_hint` says the selector is DirectSound-only. Without
///   this, the historic un-fixed probe stripped the resolver's enriched
///   error back to a bare `device not found`, hiding the ONLY
///   actionable remediation for a DirectSound-only mic in both the
///   `devices test <NAME>` CLI verb AND the Settings "Test Device"
///   action.
///
/// Pure helper: no I/O, no cpal, no env vars. Regression-tested against
/// pre-fix behavior in `device_probe_tests.rs`.
///
/// The DirectSound hint is extracted from the `resolve_error_msg`
/// itself via [`extract_directsound_hint_from_error`] rather than
/// re-queried from cpal: the resolver already ran the enumeration
/// exactly once, and a re-query risks a hot-plug race where the
/// hint appears (or disappears) between the resolver and probe calls
/// — Codex P2 on `device_probe.rs:238` (PR #669).
pub(crate) fn probe_reason_for_resolve_error(resolve_error_msg: &str) -> String {
    if resolve_error_msg.contains("no default input device available") {
        return "no default input device available".to_owned();
    }
    if resolve_error_msg.starts_with("input device not found: ") {
        let hint = extract_directsound_hint_from_error(resolve_error_msg);
        return match hint {
            Some(h) => format!("device not found{h}"),
            None => "device not found".to_owned(),
        };
    }
    // Any other error (backend outage, unexpected wording) passes through
    // verbatim so investigations still see the underlying cause.
    resolve_error_msg.to_owned()
}

/// Extract the `"; note: ...instead"` DirectSound remediation fragment
/// from a `hosts::resolve_input` error message, if present. The
/// resolver builds the fragment via [`crate::audio::hosts::directsound_only_hint`]
/// and embeds it verbatim in the aggregate "input device not found"
/// error, so parsing it back out is a stable round-trip — no second
/// DirectSound enumeration required.
///
/// Returns `None` when the message carries no hint (non-Windows,
/// unmatched selector, or the resolver simply didn't add one). Pure
/// helper so the round-trip is exhaustively unit-testable.
pub(crate) fn extract_directsound_hint_from_error(resolve_error_msg: &str) -> Option<String> {
    // The hint always starts with the exact literal `"; note: "` (see
    // `hosts::directsound_only_hint`) and ends with the resolver's
    // closing `)` — trim the closing paren off so the fragment is
    // reusable as-is in the probe reason.
    let start = resolve_error_msg.find("; note: ")?;
    let after = &resolve_error_msg[start..];
    // Take everything from `; note: ` up to (but not including) the
    // final `)` that closes the resolver's aggregate error, so the
    // fragment stays parenthesis-balanced when re-embedded.
    let end = after.rfind(')')?;
    // Guard against pathological inputs where `end` precedes `start`
    // (shouldn't happen given the message shape, but the slice must
    // still be well-formed).
    if end == 0 {
        return None;
    }
    Some(after[..end].to_owned())
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

    // Cross-host resolve: default host first, then fall through to the
    // rest of `cpal::available_hosts()`. This mirrors what live capture
    // now does (`audio::capture::start_capture`) so the probe reports
    // exactly which host would open the device.
    let ResolvedInput {
        device,
        host_id: _,
        host_label,
    } = match resolve_input(trimmed) {
        Ok(r) => r,
        Err(err) => {
            let reason = probe_reason_for_resolve_error(&err.to_string());
            return DeviceProbeResult::fail(requested_label, reason);
        }
    };
    let endpoint_token = if trimmed.is_empty() {
        default_endpoint_token().to_owned()
    } else {
        endpoint_token_for_host(host_label)
    };

    // cpal 0.18 removed `Device::name()` in favour of the `Display` impl;
    // `to_string()` is equivalent on every backend.
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
        &endpoint_token,
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
#[path = "device_probe_tests.rs"]
mod tests;
