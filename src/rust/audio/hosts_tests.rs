//! Tests for [`crate::audio::hosts`]. Companion `_tests.rs` (rather than
//! an inline `mod tests`) so the regression-test discipline scanner
//! (`src/tests/python/test_regression_test_discipline.py`) sees a
//! matching test file alongside the new module.
//!
//! The multi-host resolver walks live cpal hosts, so full coverage
//! requires an actual audio backend. What we CAN test cross-platform:
//!
//! * [`preferred_host_order`] never omits the default host and never
//!   duplicates it — this is the invariant the capture path relies on
//!   for "default first, fall through to the rest".
//! * The Windows DirectSound hint only surfaces when the requested
//!   selector actually matches an enumerated DirectSound-only name.
//! * The aggregate "not found" error message keeps its stable prefix
//!   (`input device not found: "…"`) so tests / grep in the runtime
//!   log keep working after this refactor.
//!
//! Live-host coverage (real cpal enumeration on Windows / Linux) lives
//! in the existing `audio::self_test` runner and the `dictate-run`
//! integration path; unit tests deliberately do not open cpal streams.

use super::*;

#[test]
fn preferred_host_order_starts_with_the_default_host() {
    // Regardless of which cpal features are compiled in, the platform
    // default host must be first — the capture resolver depends on this
    // to preserve the pre-refactor "same device as before" outcome for
    // users whose mic IS on the default host.
    let order = preferred_host_order();
    assert!(!order.is_empty(), "expected at least one host");
    let default_id = cpal::default_host().id();
    assert_eq!(order[0], default_id, "default host must come first");
}

#[test]
fn preferred_host_order_is_deduplicated() {
    // A host that happens to be the default must NOT appear twice in
    // the walk — a duplicate would waste an enumeration pass and, more
    // importantly, could double-count devices in the aggregate error
    // message.
    let order = preferred_host_order();
    let mut sorted = order.clone();
    sorted.sort_by_key(|id| id.name());
    sorted.dedup_by_key(|id| id.name());
    assert_eq!(order.len(), sorted.len(), "host order contains duplicates");
}

#[test]
fn snapshot_all_hosts_returns_at_least_the_default() {
    // A live cpal build always has SOME default host — even on a
    // headless dev container the null / alsa host answers construction.
    let snapshots = snapshot_all_hosts();
    assert!(
        !snapshots.is_empty(),
        "expected at least the default host in snapshot"
    );
    let default_id = cpal::default_host().id();
    assert_eq!(snapshots[0].host_id, default_id);
    assert_eq!(snapshots[0].host_label, default_id.name());
}

#[test]
fn resolve_input_empty_selector_hits_the_default_host() {
    // Empty selector = "use the default input on the default host". On
    // a headless CI box this may fail with "no default input device
    // available", which is still a well-formed error (not a panic).
    match resolve_input("") {
        Ok(resolved) => {
            let default_id = cpal::default_host().id();
            assert_eq!(
                resolved.host_id, default_id,
                "empty selector must resolve on the default host"
            );
        }
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains("no default input device available"),
                "unexpected empty-selector error: {msg}"
            );
        }
    }
}

#[test]
fn resolve_input_missing_name_error_mentions_selector_and_host_count() {
    // Pin the error-message shape the runtime log grep-tools look for.
    // The `"input device not found: "` prefix is load-bearing and used
    // by rust_session_sink's fallback logging.
    let result = resolve_input("__whisper_dictate_absolutely_missing_mic__");
    // `ResolvedInput` intentionally omits `Debug` (it wraps a `cpal::Device`
    // which doesn't derive it), so `.unwrap_err()` won't compile — pattern
    // match instead.
    let err = match result {
        Ok(_) => panic!("__whisper_dictate_absolutely_missing_mic__ resolved to a real device"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.starts_with("input device not found: "),
        "prefix drifted, message was: {msg}"
    );
    assert!(
        msg.contains("__whisper_dictate_absolutely_missing_mic__"),
        "selector should be quoted in the error: {msg}"
    );
    // At least one host was tried (the default one is always present).
    assert!(
        msg.contains("host(s)"),
        "expected host-count breadcrumb in error: {msg}"
    );
}

#[cfg(windows)]
#[test]
fn directsound_only_hint_returns_none_for_a_name_no_directsound_endpoint_uses() {
    // A never-seen name must not synthesise a DirectSound hint. The
    // check is defensive — an accidental "always Some" here would
    // gaslight every Windows user into thinking their mic is
    // DirectSound-only.
    let hint = directsound_only_hint("__whisper_dictate_absolutely_missing_mic__");
    assert!(hint.is_none());
}

#[cfg(not(windows))]
#[test]
fn directsound_only_hint_is_always_none_on_non_windows() {
    // DirectSound doesn't exist off Windows — the hint MUST NOT
    // surface, even for a name that would match on Windows.
    assert!(directsound_only_hint("Microphone (Yeti Classic)").is_none());
    assert!(directsound_only_hint("anything").is_none());
    assert!(directsound_only_hint("").is_none());
}

// ----- Codex P2 threads on PR #663 ------------------------------------------
//
// The four `resolve_input`-focused threads (hosts.rs:148 / 153 / 170) all
// exercise the same private walk. Live cpal hosts differ per box, so the
// pure `build_not_found_error` helper is the single point where the
// aggregate wording — hosts consulted, numeric-range note, DirectSound
// hint — is composable in a test without a real backend. Live-host
// coverage still comes from `audio::self_test` and `dictate-run`.

/// Test-only helper that mirrors what [`resolve_input`] would push into
/// [`build_not_found_error`] for a given synthetic host constellation.
/// Keeps the tests decoupled from cpal's live enumeration so the
/// wording is pinned deterministically on every CI box.
fn snapshot(label: &'static str, names: &[&str]) -> HostSnapshot {
    // A real HostId is only obtainable from cpal; reuse the default
    // host's id since the field isn't inspected by the error wording.
    HostSnapshot {
        host_id: cpal::default_host().id(),
        host_label: label,
        device_names: names.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn not_found_error_lists_every_searched_host_and_its_device_count() {
    // Regression for the "no hosts breadcrumb" arm of the aggregate
    // error: the message MUST carry both the total device count and
    // every consulted host label so an audio-backend investigation
    // starts with concrete counts, not a bare "not found".
    let snaps = vec![
        snapshot("WASAPI", &["Mic A", "Mic B"]),
        snapshot("ASIO", &["Studio Mic"]),
    ];
    let err = build_not_found_error("nonexistent", &snaps, None);
    let msg = err.to_string();
    assert!(msg.starts_with("input device not found: "), "prefix: {msg}");
    assert!(msg.contains("searched 3 device(s)"), "counts: {msg}");
    assert!(msg.contains("2 host(s)"), "host count: {msg}");
    assert!(msg.contains("WASAPI"), "WASAPI label: {msg}");
    assert!(msg.contains("ASIO"), "ASIO label: {msg}");
}

#[test]
fn not_found_error_folds_the_numeric_note_verbatim() {
    // The numeric-index rejection carries an actionable "pick by name
    // instead" nudge. It must reach the aggregate error verbatim so the
    // user sees the remediation in ONE line rather than having to
    // correlate two log statements.
    let snaps = vec![snapshot("WASAPI", &["Mic A", "Mic B"])];
    let note = Some(String::from(
        "index 5 out of range on default host WASAPI (2 device(s)); \
         numeric selectors resolve only against the default host - \
         pick a device by name instead",
    ));
    let err = build_not_found_error("5", &snaps, note);
    let msg = err.to_string();
    assert!(
        msg.contains("index 5 out of range"),
        "numeric note missing: {msg}"
    );
    assert!(
        msg.contains("pick a device by name instead"),
        "remediation missing: {msg}"
    );
}

#[test]
fn not_found_error_without_numeric_note_reads_cleanly() {
    // Name-only lookups shouldn't dangle a numeric-note artifact into
    // the aggregate error. Empty note = clean wording.
    let snaps = vec![snapshot("WASAPI", &["Mic A"])];
    let err = build_not_found_error("Ghost", &snaps, None);
    let msg = err.to_string();
    assert!(
        !msg.contains("numeric-index"),
        "unexpected numeric artifact: {msg}"
    );
    assert!(!msg.contains("index "), "unexpected 'index' word: {msg}");
}

// ----- fix 5 (hosts.rs:153): exact-match precedence across all hosts --------
//
// The exact/substring/numeric passes in `resolve_input` are extracted
// into the pure [`resolve_over_host_names`] helper so the precedence
// contract can be exercised against synthetic host constellations. Every
// assertion here would FAIL on the pre-fix code path where the walk
// iterated hosts in order, returning the FIRST host's substring hit
// before ever checking a later host for exact-match.

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn exact_match_on_secondary_host_beats_substring_on_default_host() {
    // Codex P2 (hosts.rs:153) scenario: default host (WASAPI) exposes
    // "USB Mic"; secondary host (ASIO) exposes "USB Mic ASIO". A
    // selector of "USB Mic ASIO" MUST resolve to the ASIO entry — NOT
    // to the default host's "USB Mic", even though "USB Mic" is a
    // substring of the selector AND the default host is tried first.
    //
    // Pre-fix behavior: `resolve_device_index` on the default host
    // returned Matched(1) for "USB Mic ASIO" via bidirectional
    // substring, and `resolve_input` returned before checking ASIO's
    // exact match. Fix: exact match across all hosts wins BEFORE any
    // substring match on any host.
    let hosts = vec![
        names(&["Realtek HD", "USB Mic"]), // WASAPI (default, index 0)
        names(&["USB Mic ASIO"]),          // ASIO (secondary, index 1)
    ];
    let outcome = resolve_over_host_names(&hosts, "USB Mic ASIO", "WASAPI");
    assert_eq!(
        outcome,
        SelectorOutcome::Matched { host: 1, device: 0 },
        "exact match on secondary host must win over substring on default"
    );
}

#[test]
fn substring_pass_still_prefers_longest_across_hosts() {
    // With no exact match anywhere, the substring pass pools across all
    // hosts and picks the LONGEST name. Same longest-wins tiebreak the
    // capture path's single-host resolver uses — just pooled across
    // hosts.
    let hosts = vec![
        names(&["Headset Microphone"]),
        names(&["Headset Microphone (Jabra Evolve 65 TE)"]),
    ];
    let outcome = resolve_over_host_names(&hosts, "Headset Microphone (Jabra Evolv", "WASAPI");
    assert_eq!(
        outcome,
        SelectorOutcome::Matched { host: 1, device: 0 },
        "longest substring match must win irrespective of host order"
    );
}

#[test]
fn default_host_wins_when_both_hosts_have_the_same_exact_name() {
    // Ties on exact match resolve in preferred_host_order — the default
    // host (index 0) always wins the tie so users who never see a
    // secondary host keep the pre-refactor "same device as before"
    // outcome.
    let hosts = vec![names(&["USB Mic"]), names(&["USB Mic"])];
    assert_eq!(
        resolve_over_host_names(&hosts, "USB Mic", "WASAPI"),
        SelectorOutcome::Matched { host: 0, device: 0 }
    );
}

#[test]
fn empty_selector_never_matches_via_substring_or_exact() {
    // Empty needle would otherwise contains-match every device via the
    // empty-substring rule. Guarded in the resolver so the empty-
    // selector default-host branch is the only path an empty selector
    // ever takes.
    let hosts = vec![names(&["USB Mic"]), names(&["Another Mic"])];
    assert_eq!(
        resolve_over_host_names(&hosts, "", "WASAPI"),
        SelectorOutcome::NotFound
    );
    assert_eq!(
        resolve_over_host_names(&hosts, "   ", "WASAPI"),
        SelectorOutcome::NotFound
    );
}

// ----- fix 3 (hosts.rs:170): numeric selectors stay in the published index --

#[test]
fn numeric_selector_out_of_range_on_default_host_returns_actionable_note() {
    // Codex P2 (hosts.rs:170) scenario: numeric selector "5" is out of
    // range on the default host (which has 2 mics) BUT would be valid
    // as an index into a secondary host if the pre-fix behavior of
    // walking every host applied. The fix rejects the number outright
    // with an actionable note - pick by name instead.
    //
    // Device names deliberately contain NO digits, so the selector
    // never resolves via the substring pass — the numeric-index
    // fallback is the ONLY code path under test here.
    //
    // Pre-fix behavior: the walk continued past the default host and
    // opened `hosts[1].nth(idx)` (whichever host had a matching numeric
    // range), silently opening an unrelated mic. Post-fix: numeric
    // selectors resolve ONLY against `hosts[0]`.
    let hosts = vec![
        names(&["Realtek HD", "USB Headset"]),
        names(&[
            "ASIO Studio A",
            "ASIO Studio B",
            "ASIO Studio C",
            "ASIO Studio D",
            "ASIO Studio E",
            "ASIO Studio F",
        ]),
    ];
    let outcome = resolve_over_host_names(&hosts, "5", "WASAPI");
    match outcome {
        SelectorOutcome::NumericOutOfRange { note } => {
            assert!(
                note.contains("index 5 out of range"),
                "range detail missing: {note}"
            );
            assert!(
                note.contains("default host WASAPI"),
                "default-host label missing: {note}"
            );
            assert!(
                note.contains("pick a device by name instead"),
                "actionable remediation missing: {note}"
            );
        }
        other => panic!(
            "numeric selector out of range on default host must NOT resolve, \
             got {other:?}"
        ),
    }
}

#[test]
fn numeric_selector_in_range_on_default_host_matches_that_host() {
    // Positive case: a numeric selector inside the default host's
    // range resolves normally against the default host (no digits in
    // any device name so the substring pass does not steal the match).
    let hosts = vec![
        names(&["Mic A", "Mic B", "Mic C"]),
        names(&["ASIO Studio A"]),
    ];
    assert_eq!(
        resolve_over_host_names(&hosts, "1", "WASAPI"),
        SelectorOutcome::Matched { host: 0, device: 1 }
    );
}

#[test]
fn numeric_selector_never_probes_secondary_hosts() {
    // Load-bearing invariant for fix 3: even if a secondary host would
    // accept the numeric selector cleanly, the resolver MUST NOT open
    // it via a number. Pre-fix code returned `Matched(secondary,
    // idx)` here; post-fix returns `NumericOutOfRange`. Digit-free
    // names so the substring pass can't intervene.
    let hosts = vec![
        names(&["Only Default Mic"]), // default host has 1 device
        names(&["ASIO One", "ASIO Two", "ASIO Three"]),
    ];
    // "2" would validly index into the ASIO host (device 2) but is out
    // of range on the default host (only 1 device).
    let outcome = resolve_over_host_names(&hosts, "2", "WASAPI");
    assert!(
        matches!(outcome, SelectorOutcome::NumericOutOfRange { .. }),
        "numeric selector must never fall through to a secondary host, \
         got {outcome:?}"
    );
}

// ----- fix 4 (hosts.rs:148): propagate host enumeration failures ------------

#[test]
fn no_searchable_hosts_error_prefix_marks_the_enumeration_failure_path() {
    // Codex P2 (hosts.rs:148) scenario: no host successfully
    // enumerated. Pre-fix behavior: `resolve_input` returned "input
    // device not found: ... (searched 0 device(s) across 0 host(s):
    // no hosts)" — indistinguishable from a bad saved mic name.
    // Post-fix: a DISTINCT error prefix ("enumerate input devices: ")
    // so an audio-backend outage is separable from a name miss in the
    // runtime log.
    let msg = no_searchable_hosts_error_message(&[String::from(
        "host WASAPI: input_devices() failed (permission denied)",
    )]);
    assert!(
        msg.starts_with("enumerate input devices: "),
        "distinct enumeration-failure prefix missing: {msg}"
    );
    assert!(
        msg.contains("permission denied"),
        "underlying host error must be preserved: {msg}"
    );
    // Critically, this must NOT masquerade as a name-lookup miss.
    assert!(
        !msg.starts_with("input device not found:"),
        "enumeration failure must not present as a name miss: {msg}"
    );
}

#[test]
fn no_searchable_hosts_error_joins_multiple_underlying_causes() {
    // When both the default host and every secondary host fail, the
    // error must carry ALL causes joined by "; " so a diagnostic sees
    // whether the outage is host-wide (e.g. audio server down) or
    // isolated to one backend.
    let msg = no_searchable_hosts_error_message(&[
        String::from("host WASAPI: input_devices() failed (E1)"),
        String::from("host ASIO: constructor failed (E2)"),
    ]);
    assert!(msg.contains("E1"), "first cause missing: {msg}");
    assert!(msg.contains("E2"), "second cause missing: {msg}");
    assert!(msg.contains("; "), "causes must be joined by '; ': {msg}");
}

#[test]
fn no_searchable_hosts_error_falls_back_to_generic_reason_when_empty() {
    // Defensive: if the caller somehow got an empty error slice (no
    // host reported an error, yet no host is searchable), still emit
    // the distinctive enumeration-failure prefix so callers don't
    // misclassify it as a name miss.
    let msg = no_searchable_hosts_error_message(&[]);
    assert!(msg.starts_with("enumerate input devices: "));
    assert!(msg.contains("no cpal hosts available"));
}

#[test]
fn resolve_input_missing_name_still_uses_the_name_not_found_prefix() {
    // The complementary invariant: a name that fails to resolve
    // against successfully-enumerated hosts MUST still use the
    // historic "input device not found: " prefix (the runtime log
    // grep-tools look for it, see rust_session_sink). Fix 4 must not
    // spill the enumeration-failure prefix onto the name-miss path.
    let result = resolve_input("__whisper_dictate_definitely_missing_mic_fix4__");
    let msg = match result {
        Ok(_) => panic!("synthetic mic name unexpectedly resolved"),
        Err(e) => e.to_string(),
    };
    // On a headless dev box with no cpal hosts, the enumeration
    // failure path may fire instead — accept either shape, but each
    // MUST have its own distinctive prefix.
    let is_name_miss = msg.starts_with("input device not found: ");
    let is_enum_failure = msg.starts_with("enumerate input devices: ");
    assert!(
        is_name_miss || is_enum_failure,
        "unexpected error shape: {msg}"
    );
    // Whichever prefix fired, the OTHER one must NOT appear — the
    // whole point of fix 4 is that the two states are separable.
    if is_name_miss {
        assert!(
            !msg.starts_with("enumerate input devices: "),
            "name-miss branch must not spill enumeration-failure prefix: {msg}"
        );
    }
}
