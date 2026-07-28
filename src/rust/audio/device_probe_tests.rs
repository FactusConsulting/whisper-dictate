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

// ----- Codex P2 (#663, #669): DirectSound hint round-trips via error ---------
//
// `probe_reason_for_resolve_error` extracts the DirectSound remediation
// from the resolver's own error message (see
// `extract_directsound_hint_from_error`) rather than re-querying
// `hosts::directsound_only_hint`. The pure helpers below exercise both
// halves of the round-trip against synthetic error strings.

#[test]
fn probe_reason_preserves_directsound_hint_when_present_in_error_message() {
    // #663 regression pin, updated for the #669 no-re-query design.
    // When the resolver embedded its enriched "pick the WASAPI variant"
    // hint into the aggregate error, the probe MUST preserve it in the
    // short "device not found" reason — the ONLY actionable remediation
    // for a DirectSound-only mic.
    let synthetic_error = concat!(
        "input device not found: \"Blue Yeti\" ",
        "(searched 0 device(s) across 1 host(s): WASAPI",
        "; note: \"Blue Yeti\" is only visible via Windows DirectSound, ",
        "which cpal 0.18 cannot open - pick the WASAPI variant in the mic ",
        "picker instead)",
    );
    let reason = probe_reason_for_resolve_error(synthetic_error);
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
fn probe_reason_stays_bare_when_error_carries_no_directsound_hint() {
    // The other side of the pin: when the resolver's error carries no
    // hint (non-Windows, unmatched selector, or the resolver simply
    // didn't add one) the probe reason MUST be the bare short "device
    // not found" string. Otherwise every never-seen name on non-Windows
    // would gaslight the user with a spurious DirectSound message.
    let synthetic_error =
        r#"input device not found: "Ghost" (searched 3 device(s) across 1 host(s): ALSA)"#;
    let reason = probe_reason_for_resolve_error(synthetic_error);
    assert_eq!(reason, "device not found");
}

#[test]
fn probe_reason_preserves_no_default_wording_verbatim() {
    // The empty-selector "no default input available" case is a
    // separate short reason that must NOT accrete a DirectSound hint
    // (the hint is a name-lookup remediation; the empty-selector
    // branch never involves a name lookup).
    let synthetic_error = "no default input device available";
    let reason = probe_reason_for_resolve_error(synthetic_error);
    assert_eq!(reason, "no default input device available");
}

#[test]
fn probe_reason_passes_through_unexpected_wording() {
    // A resolver error the probe doesn't recognise (backend outage,
    // future wording drift) MUST reach the UI verbatim so an
    // investigation still sees the underlying cause instead of a
    // silent "device not found" that hides it.
    let synthetic_error = "enumerate input devices: ALSA: permission denied";
    let reason = probe_reason_for_resolve_error(synthetic_error);
    assert_eq!(reason, "enumerate input devices: ALSA: permission denied");
}

// ----- Codex P2 (#669 device_probe.rs:238): hint extraction round-trip ------
//
// `extract_directsound_hint_from_error` is the single hop that
// makes the "no re-query" design work: it turns the resolver's own
// error message back into the exact hint fragment
// `hosts::directsound_only_hint` produced. Test the round-trip
// directly so drift on either end (resolver format change OR probe
// parser change) surfaces as a test failure rather than a silent
// diagnostic-loss regression.

#[test]
fn extract_directsound_hint_recovers_the_resolver_fragment_verbatim() {
    // Codex P2 (#669 device_probe.rs:238) regression pin. The resolver
    // embeds the hint via `; note: ...instead)` inside the aggregate
    // error; the probe extractor MUST recover the fragment (WITHOUT
    // the trailing closing paren) so it can be re-embedded without
    // double-closing the parens.
    let synthetic_error = concat!(
        "input device not found: \"Blue Yeti\" ",
        "(searched 0 device(s) across 1 host(s): WASAPI",
        "; note: \"Blue Yeti\" is only visible via Windows DirectSound, ",
        "which cpal 0.18 cannot open - pick the WASAPI variant in the mic ",
        "picker instead)",
    );
    let hint =
        extract_directsound_hint_from_error(synthetic_error).expect("hint must be recovered");
    assert!(
        hint.starts_with("; note: "),
        "hint must keep the leading '; note: ' delimiter so it re-embeds \
         cleanly onto 'device not found': {hint}"
    );
    assert!(
        hint.ends_with("instead"),
        "hint must NOT carry the resolver's closing paren (double-close): {hint}"
    );
    assert!(
        hint.contains("pick the WASAPI variant"),
        "actionable remediation must survive: {hint}"
    );
}

#[test]
fn extract_directsound_hint_returns_none_when_error_carries_no_note() {
    // Non-Windows and Windows-with-unmatched-selector: no hint in the
    // error message. Extractor MUST return None so the probe stays on
    // the bare "device not found" path.
    let synthetic_error =
        r#"input device not found: "Ghost" (searched 3 device(s) across 1 host(s): ALSA)"#;
    assert!(extract_directsound_hint_from_error(synthetic_error).is_none());
    // Other error shapes (backend outage, enumeration failure) never
    // carry the hint either.
    assert!(extract_directsound_hint_from_error(
        "enumerate input devices: ALSA: permission denied"
    )
    .is_none());
    assert!(extract_directsound_hint_from_error("no default input device available").is_none());
    assert!(extract_directsound_hint_from_error("").is_none());
}

#[test]
fn extract_directsound_hint_ignores_selector_containing_generic_note_delimiter() {
    // Codex P2 (#669 device_probe.rs:225) regression pin. A Windows
    // device can be user-renamed to contain the literal `; note: `
    // sequence. The selector is embedded near the beginning of the
    // aggregate error, so the pre-fix `find("; note: ")` variant would
    // have mis-sliced the entire message from the SELECTOR's `; note:
    // ` through the closing paren — surfacing corrupted host-search
    // text as the probe reason. Fix: anchor on the distinctive
    // marker text unique to the resolver-generated hint.
    let synthetic_error = concat!(
        r#"input device not found: "Studio; note: Mic" "#,
        "(searched 3 device(s) across 1 host(s): WASAPI)",
    );
    // There is NO real DirectSound hint in this error — the "; note: "
    // sequence appears ONLY inside the selector. Extractor MUST return
    // None so the probe stays on the bare "device not found" path.
    let hint = extract_directsound_hint_from_error(synthetic_error);
    assert!(
        hint.is_none(),
        "extractor must not treat a selector's '; note: ' as the DirectSound hint: {hint:?}"
    );
    // And the probe reason for such a miss stays the short bare string.
    let reason = probe_reason_for_resolve_error(synthetic_error);
    assert_eq!(reason, "device not found");
}

#[test]
fn extract_directsound_hint_still_recovers_when_selector_also_contains_note_delimiter() {
    // Complementary pin: even when the selector contains "; note: ",
    // an ACTUAL DirectSound hint at the end of the error must still
    // be recovered correctly. Anchor-then-walk-back means the earlier
    // `; note: ` in the selector doesn't derail the extraction.
    let synthetic_error = concat!(
        r#"input device not found: "Studio; note: Mic" "#,
        "(searched 0 device(s) across 1 host(s): WASAPI",
        "; note: \"Studio; note: Mic\" is only visible via Windows DirectSound, ",
        "which cpal 0.18 cannot open - pick the WASAPI variant in the mic ",
        "picker instead)",
    );
    let hint = extract_directsound_hint_from_error(synthetic_error)
        .expect("real hint at end of error must be recovered");
    // Anchoring on the distinctive marker means we walk BACK from it
    // to the correct "; note: " (the one that introduces the hint,
    // not the one inside the selector).
    assert!(
        hint.contains(DIRECTSOUND_HINT_MARKER),
        "extracted fragment must include the distinctive marker: {hint}"
    );
    assert!(
        hint.contains("pick the WASAPI variant"),
        "actionable remediation must survive: {hint}"
    );
    assert!(
        hint.ends_with("instead"),
        "extracted fragment must not carry the resolver's closing paren: {hint}"
    );
}

// ----- Claude Copilot review (#669 device_probe.rs:198): numeric OOR --------
// The resolver emits a NumericOutOfRange note inside the same
// "input device not found: ..." wrapper as a plain name miss (see
// `hosts::build_not_found_error`). The probe used to squash BOTH cases
// to a bare "device not found", losing the actionable "pick by name"
// remediation for a stale numeric setting. Fix: extract the numeric
// note and surface it verbatim as the probe reason.

#[test]
fn probe_reason_preserves_numeric_note_when_selector_is_out_of_range() {
    // Regression pin. Given the resolver's numeric-out-of-range error
    // wrapper, the probe MUST surface the actionable remediation text
    // - not the bare "device not found" it emitted pre-fix.
    let synthetic_error = concat!(
        r#"input device not found: "5" "#,
        "(searched 3 device(s) across 1 host(s): WASAPI",
        "; index 5 out of range on default host WASAPI (3 device(s)); ",
        "numeric selectors resolve only against the default host - ",
        "pick a device by name instead",
        ")",
    );
    let reason = probe_reason_for_resolve_error(synthetic_error);
    assert!(
        reason.contains("index 5 out of range"),
        "numeric range detail must survive: {reason}"
    );
    assert!(
        reason.contains("pick a device by name instead"),
        "actionable remediation must survive: {reason}"
    );
    // And the reason must NOT be the bare "device not found" the
    // pre-fix probe would have emitted.
    assert_ne!(
        reason, "device not found",
        "numeric-out-of-range case must not squash to bare 'device not found'"
    );
}

#[test]
fn extract_numeric_note_returns_none_for_plain_name_miss() {
    // The extractor MUST NOT fire on a plain name miss - only on the
    // resolver's numeric-out-of-range wrapper. A device named
    // "out of range" is a synthetic edge case, but the marker also
    // requires "on default host" as the disambiguator.
    let plain_miss =
        r#"input device not found: "Ghost" (searched 3 device(s) across 1 host(s): WASAPI)"#;
    assert!(extract_numeric_note_from_error(plain_miss).is_none());
    // Even a device NAMED "out of range" doesn't mis-fire, because
    // the marker requires "out of range on default host".
    let device_named_out_of_range = concat!(
        r#"input device not found: "out of range" "#,
        "(searched 3 device(s) across 1 host(s): WASAPI)",
    );
    assert!(extract_numeric_note_from_error(device_named_out_of_range).is_none());
}

#[test]
fn extract_numeric_note_returns_none_when_error_carries_only_directsound_hint() {
    // When only the DirectSound hint is present (no numeric note),
    // the extractor MUST return None so `probe_reason_for_resolve_error`
    // falls through to the DirectSound-hint branch.
    let synthetic_error = concat!(
        r#"input device not found: "Blue Yeti" "#,
        "(searched 0 device(s) across 1 host(s): WASAPI",
        "; note: \"Blue Yeti\" is only visible via Windows DirectSound, ",
        "which cpal 0.18 cannot open - pick the WASAPI variant in the mic ",
        "picker instead)",
    );
    assert!(extract_numeric_note_from_error(synthetic_error).is_none());
}

#[test]
fn extract_numeric_note_prefers_numeric_when_both_notes_are_present() {
    // Corner case: if BOTH notes fire in the same error (numeric
    // out-of-range PLUS DirectSound hint), the extractor MUST return
    // ONLY the numeric portion — the DirectSound hint's " ; note: ..."
    // fragment must not leak into the extracted note.
    let synthetic_error = concat!(
        r#"input device not found: "5" "#,
        "(searched 3 device(s) across 1 host(s): WASAPI",
        "; index 5 out of range on default host WASAPI (3 device(s)); ",
        "numeric selectors resolve only against the default host - ",
        "pick a device by name instead",
        "; note: \"5\" is only visible via Windows DirectSound, ",
        "which cpal 0.18 cannot open - pick the WASAPI variant in the mic ",
        "picker instead)",
    );
    let note =
        extract_numeric_note_from_error(synthetic_error).expect("numeric note must be extracted");
    assert!(
        note.contains("out of range on default host"),
        "numeric portion must be present: {note}"
    );
    assert!(
        !note.contains("Windows DirectSound"),
        "DirectSound hint must NOT leak into the extracted numeric note: {note}"
    );
    // The extracted note ends exactly at the numeric closing anchor.
    assert!(
        note.ends_with(NUMERIC_OOR_NOTE_END),
        "extracted note must end at the numeric-remediation anchor: {note}"
    );
}

#[test]
fn extract_directsound_hint_survives_a_numeric_note_prefix() {
    // The resolver may combine the numeric-out-of-range note with the
    // DirectSound hint in the same aggregate error. The extractor must
    // pick up the DirectSound `; note: ...instead)` fragment even when
    // an earlier semicolon-separated note is present.
    let synthetic_error = concat!(
        "input device not found: \"5\" ",
        "(searched 3 device(s) across 1 host(s): WASAPI",
        "; index 5 out of range on default host WASAPI (3 device(s)); ",
        "numeric selectors resolve only against the default host - ",
        "pick a device by name instead",
        "; note: \"5\" is only visible via Windows DirectSound, ",
        "which cpal 0.18 cannot open - pick the WASAPI variant in the mic ",
        "picker instead)",
    );
    let hint = extract_directsound_hint_from_error(synthetic_error)
        .expect("extractor must recover the DirectSound fragment");
    assert!(
        hint.starts_with("; note: "),
        "expected the DirectSound note-fragment start, got: {hint}"
    );
    assert!(
        hint.contains("pick the WASAPI variant"),
        "actionable remediation must survive: {hint}"
    );
    // Critically, the numeric-note portion MUST NOT bleed into the
    // extracted hint (it belongs to the outer error, not to the
    // DirectSound remediation).
    assert!(
        !hint.contains("out of range"),
        "numeric note must not leak into the DirectSound hint: {hint}"
    );
}
