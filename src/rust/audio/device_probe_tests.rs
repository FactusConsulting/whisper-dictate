//! Tests for [`crate::audio::device_probe`]. Companion `_tests.rs` (rather
//! than an inline `mod tests`) so the regression-test discipline scanner
//! (`src/tests/python/test_regression_test_discipline.py`) sees a matching
//! test file alongside the module.

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
fn endpoint_token_for_host_maps_all_known_cpal_labels() {
    // The UI parser (`crate::ui::device_test::endpoint_label`) renders
    // whatever lowercase token we emit; sounddevice historically used
    // the vocabulary below so this locks the mapping in place. A new
    // cpal host falls through as its own lowercased label so the
    // probe still emits something inspectable.
    assert_eq!(endpoint_token_for_host("WASAPI"), "wasapi");
    assert_eq!(endpoint_token_for_host("ASIO"), "asio");
    assert_eq!(endpoint_token_for_host("ALSA"), "alsa");
    assert_eq!(endpoint_token_for_host("PulseAudio"), "pulseaudio");
    assert_eq!(endpoint_token_for_host("PipeWire"), "pipewire");
    assert_eq!(endpoint_token_for_host("JACK"), "jack");
    assert_eq!(endpoint_token_for_host("CoreAudio"), "coreaudio");
    // Fallback: unknown label passes through lowercased so a future
    // cpal host is still identifiable from the JSON envelope.
    assert_eq!(endpoint_token_for_host("MysteryHost"), "mysteryhost");
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
    let json: serde_json::Value = serde_json::from_str(&result.to_json_line()).expect("valid JSON");
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
    let json: serde_json::Value = serde_json::from_str(&result.to_json_line()).expect("valid JSON");
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
    let json =
        DeviceProbeResult::ok("Mic".to_owned(), "default", 48_000, "float32", true).to_json_line();
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
    // — that's what the UI's inline ✗ + reason renders. The DirectSound
    // hint only appends when the selector matches a DirectSound-only
    // endpoint, so a synthetic never-seen name must NOT accrete the hint
    // (see `probe_directsound_hint_only_appended_when_endpoint_matches`).
    let r = probe_device("__whisper_dictate_definitely_missing_device__");
    assert!(!r.usable);
    // The bare-prefix invariant: reason ALWAYS starts with "device not
    // found" so the UI's grep-based renderer still classifies the failure
    // even when the DirectSound hint appends after it.
    let reason = r.reason.as_deref().expect("failure carries a reason");
    assert!(
        reason.starts_with("device not found"),
        "unexpected reason prefix: {reason}"
    );
    // On non-Windows there IS no DirectSound at all, so the reason must
    // be the bare short string. On Windows a name that isn't a real
    // DirectSound endpoint (the case here) must also not surface the
    // hint.
    assert_eq!(reason, "device not found");
    assert!(r.endpoint.is_none());
    assert!(r.samplerate.is_none());
    assert!(r.dtype.is_none());
    assert!(!r.resampled);
}

// ----- Codex P2 (#663): preserve the DirectSound hint in probe failures ------
//
// The `probe_reason_for_resolve_error` helper is pure — no cpal, no env,
// no I/O — so we can exercise both the pre-fix behavior (the WITHOUT-hint
// branch) AND the post-fix behavior (the WITH-hint branch) with synthetic
// inputs. Every assertion here would fail on the un-fixed code path where
// the `input device not found: ...` branch unconditionally returned the
// bare `"device not found"` string.

#[test]
fn probe_reason_preserves_directsound_hint_when_hint_is_present() {
    // Fix 2 regression pin: when `hosts::resolve_input` returns its
    // enriched `input device not found: ... hint` message AND the
    // Windows DirectSound hint is present, the probe MUST append the
    // hint to the short reason. The pre-fix code unconditionally
    // returned `"device not found"` here, dropping the ONLY actionable
    // remediation the resolver adds for a DirectSound-only mic.
    let synthetic_error =
        r#"input device not found: "Blue Yeti" (searched 0 device(s) across 1 host(s): WASAPI)"#;
    let synthetic_hint = Some(String::from(
        "; note: \"Blue Yeti\" is only visible via Windows DirectSound, \
         which cpal 0.18 cannot open - pick the WASAPI variant in the mic \
         picker instead",
    ));
    let reason = probe_reason_for_resolve_error(synthetic_error, synthetic_hint);
    assert!(
        reason.starts_with("device not found"),
        "short prefix must stay stable: {reason}"
    );
    assert!(
        reason.contains("DirectSound"),
        "hint must survive into the probe reason: {reason}"
    );
    assert!(
        reason.contains("pick the WASAPI variant"),
        "actionable remediation must survive: {reason}"
    );
}

#[test]
fn probe_reason_stays_bare_when_no_directsound_hint_is_present() {
    // The other side of the pin: when there is NO DirectSound hint
    // (non-Windows, or a Windows box where the selector doesn't match a
    // DirectSound-only endpoint) the probe reason MUST be the bare
    // short "device not found" string the UI parser has always
    // rendered. Otherwise every never-seen name on non-Windows would
    // gaslight the user with a spurious DirectSound message.
    let synthetic_error =
        r#"input device not found: "Ghost" (searched 3 device(s) across 1 host(s): ALSA)"#;
    let reason = probe_reason_for_resolve_error(synthetic_error, None);
    assert_eq!(reason, "device not found");
}

#[test]
fn probe_reason_preserves_no_default_wording_verbatim() {
    // The empty-selector "no default input available" case is a
    // separate short reason that must NOT accrete a DirectSound hint
    // (the hint is a name-lookup remediation; the empty-selector
    // branch never involves a name lookup).
    let synthetic_error = "no default input device available";
    let hint = Some(String::from(
        "; note: never applies to the default-input branch",
    ));
    let reason = probe_reason_for_resolve_error(synthetic_error, hint);
    assert_eq!(reason, "no default input device available");
}

#[test]
fn probe_reason_passes_through_unexpected_wording() {
    // A resolver error the probe doesn't recognise (backend outage,
    // future wording drift) MUST reach the UI verbatim so an
    // investigation still sees the underlying cause instead of a
    // silent "device not found" that hides it.
    let synthetic_error = "enumerate input devices: ALSA: permission denied";
    let reason = probe_reason_for_resolve_error(synthetic_error, None);
    assert_eq!(reason, "enumerate input devices: ALSA: permission denied");
}

#[test]
fn probe_reason_ignores_empty_hint_string() {
    // Defensive: `directsound_only_hint` may return `Some("")` in
    // theory; treat an empty hint the same as `None` so the reason
    // stays clean.
    let synthetic_error = r#"input device not found: "Ghost" (...)"#;
    let reason = probe_reason_for_resolve_error(synthetic_error, Some(String::new()));
    assert_eq!(reason, "device not found");
}
