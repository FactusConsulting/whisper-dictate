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
    let hint = directsound_only_hint("__whisper_dictate_absolutely_missing_mic__", &[]);
    assert!(hint.is_none());
}

// ----- Codex P2 (#674 hosts.rs:661): Windows-specific picker verification ---
//
// The two tests below exercise the Windows-only DirectSound
// suppression path against LIVE cpal enumeration on the
// `rust-features (windows-2025, audio, --features audio-in-rust,
// test)` CI job. They complement the cross-platform pure-predicate
// pins by verifying the actual `#[cfg(windows)]` early-return
// branch runs correctly on real WASAPI hardware.

#[cfg(windows)]
#[test]
fn windows_directsound_hint_suppressed_when_cpal_already_enumerates_selector() {
    // Live-Windows regression: pick any name cpal actually
    // enumerates (via snapshot_all_hosts on the platform), then call
    // `directsound_only_hint` with that name as selector plus the
    // full cpal-enumerated names list as the suppression context. On
    // pre-fix code the hint would fire if DS happened to also see
    // that name (it always does for WASAPI-visible mics); post-fix
    // the cpal-name check suppresses it. On a Windows CI runner with
    // at least one input device this test exercises the actual
    // WASAPI + DirectSound enumeration end-to-end.
    let snapshots = snapshot_all_hosts();
    // Collect real cpal names across every host.
    let cpal_names: Vec<String> = snapshots
        .iter()
        .flat_map(|s| s.device_names.iter().cloned())
        .filter(|n| !n.is_empty())
        .collect();
    if cpal_names.is_empty() {
        // Headless CI runner with no mics; the branch we want to
        // verify requires at least one cpal name — skip cleanly.
        return;
    }
    let selector = cpal_names[0].clone();
    let cpal_refs: Vec<&str> = cpal_names.iter().map(|s| s.as_str()).collect();
    let hint = directsound_only_hint(&selector, &cpal_refs);
    assert!(
        hint.is_none(),
        "DirectSound hint MUST be suppressed for a selector that \
         cpal enumerated (via WASAPI on Windows). This exercises the \
         #[cfg(windows)] early-return branch of directsound_only_hint \
         end-to-end. Selector: {selector:?}"
    );
}

#[cfg(windows)]
#[test]
fn windows_snapshot_all_hosts_surfaces_wasapi_devices_with_usable_true() {
    // Live-Windows regression: the `snapshot_all_hosts` shim should
    // report WASAPI (the default host on Windows) with usable=true
    // for every enumerated name. This exercises the code path the
    // picker uses on Windows CI runners.
    let snapshots = snapshot_all_hosts();
    let default_id = cpal::default_host().id();
    let default_snap = snapshots
        .iter()
        .find(|s| s.host_id == default_id)
        .expect("default host must be present in snapshot");
    assert_eq!(
        default_snap.host_label, "WASAPI",
        "on Windows the default cpal host is WASAPI"
    );
    // If the runner has any mics, they must all report usable=true
    // in the snapshot (the shim does not run pick-config filtering —
    // it's a diagnostic listing).
    for (name, usable) in default_snap
        .device_names
        .iter()
        .zip(default_snap.usable.iter())
    {
        if name.is_empty() {
            continue;
        }
        assert!(
            *usable,
            "snapshot_all_hosts must report usable=true for every \
             enumerated name (name={name:?}); the shim is a listing, \
             not a resolver input"
        );
    }
}

#[cfg(not(windows))]
#[test]
fn directsound_only_hint_is_always_none_on_non_windows() {
    // DirectSound doesn't exist off Windows — the hint MUST NOT
    // surface, even for a name that would match on Windows.
    assert!(directsound_only_hint("Microphone (Yeti Classic)", &[]).is_none());
    assert!(directsound_only_hint("anything", &[]).is_none());
    assert!(directsound_only_hint("", &[]).is_none());
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
    // Non-empty names are treated as usable; empty-name entries are
    // usability-filtered placeholders. Enumeration succeeded (no
    // failure error).
    let device_names: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    let usable: Vec<bool> = device_names.iter().map(|n| !n.is_empty()).collect();
    HostSnapshot {
        host_id: cpal::default_host().id(),
        host_label: label,
        device_names,
        usable,
        enumeration_error: None,
    }
}

/// Snapshot for a host whose enumeration FAILED — used by the
/// hosts.rs:200 fix regression tests to check the aggregate error
/// distinguishes "searched" from "failed" hosts.
fn failed_snapshot(label: &'static str, err: &str) -> HostSnapshot {
    HostSnapshot {
        host_id: cpal::default_host().id(),
        host_label: label,
        device_names: Vec::new(),
        usable: Vec::new(),
        enumeration_error: Some(err.to_owned()),
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

// ----- Codex P2 (#669 hosts.rs:203): empty enumeration != host failure -----

#[test]
fn should_propagate_enumeration_failure_only_when_no_host_succeeded() {
    // Codex P2 (#669 hosts.rs:203) regression pin. Pre-fix code used
    // `any_searchable = host_slots.iter().any(|s| !s.names.is_empty())`
    // which conflated "no host succeeded" (backend outage → propagate
    // the enumeration-failure error) with "hosts succeeded but returned
    // zero devices" (headless box / no mics → fall through to the plain
    // 'device not found' path). This helper separates the two so the
    // headless case doesn't surface the misleading
    // `enumerate input devices: no cpal hosts available` error.
    //
    // Post-fix contract: propagate ONLY when NO host enumerated —
    // hence the predicate is precisely `!any_host_succeeded`.
    assert!(
        should_propagate_enumeration_failure(false),
        "when NO host succeeded, propagate the enumeration-failure error"
    );
    assert!(
        !should_propagate_enumeration_failure(true),
        "when ANY host succeeded (even with 0 devices), fall through to \
         the 'device not found' path — otherwise a headless box surfaces \
         a misleading verbose error for every named-device probe"
    );
}

// ----- Codex P2 (#669 hosts.rs:193): default-host identity preserved --------
//
// When the default host's enumeration fails but a secondary host
// succeeds, `host_slots[0]` used to become the SECONDARY host — so
// numeric selectors resolved against its device list (silently opening
// its nth mic) and the "out of range on default host" note quoted the
// wrong host label. Fix: always place the default host at slot 0, even
// on enumeration failure, so [`resolve_over_host_names`] indexes the
// right list and quotes the right label.

#[test]
fn numeric_selector_reports_default_host_label_even_when_default_slot_is_empty() {
    // Regression pin for the #669 hosts.rs:193 thread. Simulate the
    // partial-failure case at the pure-resolver level: hosts[0] is the
    // "real" default host but its device list is empty (mimicking a
    // failed enumeration); hosts[1] is a fully populated secondary
    // host. A numeric selector out of range on hosts[0] MUST produce
    // NumericOutOfRange with the DEFAULT host's label, not the
    // secondary host's — and MUST NOT resolve to the secondary host's
    // nth device via a silent fallback.
    let hosts = vec![
        Vec::<String>::new(), // default host (enumeration returned nothing)
        names(&["ASIO Studio One", "ASIO Studio Two", "ASIO Studio Three"]),
    ];
    let outcome = resolve_over_host_names(&hosts, "2", "WASAPI");
    match outcome {
        SelectorOutcome::NumericOutOfRange { note } => {
            assert!(
                note.contains("default host WASAPI"),
                "numeric note must quote the DEFAULT host label, not \
                 whichever host happens to sit at slot 0: {note}"
            );
            assert!(
                note.contains("0 device(s)"),
                "numeric note must reflect the actual (empty) default-host \
                 device count: {note}"
            );
        }
        SelectorOutcome::Matched { host, device } => panic!(
            "numeric selector resolved to a SECONDARY host silently \
             (host={host}, device={device}) - the wrong-device fallback \
             this fix was meant to prevent"
        ),
        other => panic!("expected NumericOutOfRange, got {other:?}"),
    }
}

#[test]
fn numeric_selector_never_opens_secondary_when_default_slot_is_empty() {
    // Complementary pin: even if the numeric index happens to be valid
    // on a secondary host, hosts[0] being empty MUST result in
    // NumericOutOfRange - never Matched(secondary, idx). The empty
    // default slot enforces the "numeric on default host only" contract.
    let hosts = vec![
        Vec::<String>::new(), // default host, empty
        names(&["ASIO Alpha", "ASIO Beta"]),
    ];
    // idx 0 IS a valid position in hosts[1], but MUST NOT resolve there.
    let outcome = resolve_over_host_names(&hosts, "0", "WASAPI");
    assert!(
        matches!(outcome, SelectorOutcome::NumericOutOfRange { .. }),
        "numeric selector must NEVER fall through from an empty \
         default-host slot to a secondary host, got {outcome:?}"
    );
}

// ----- Codex P2 (#669 hosts.rs:149): short-circuit default-host exact match -
//
// The short-circuit lives in `resolve_input` (which does its own exact-
// match check against just the default host BEFORE enumerating any
// secondary hosts), so it can't be pinned by the pure
// `resolve_over_host_names` helper. What we CAN pin is the invariant
// the short-circuit relies on: given a default-host exact match, the
// full-walk resolver would ALSO return that default-host device, so
// the short-circuit does not change observable results — only latency.
// A regression that breaks the invariant (e.g. default-host ties losing
// to a secondary) would surface here.

#[test]
fn default_host_exact_match_wins_the_full_walk_too() {
    // Belt-and-braces for the short-circuit: the full multi-host walk
    // MUST also pick the default host's exact match, so the short-
    // circuit's early return produces the SAME device the full walk
    // would have. This is what makes the perf optimization safe.
    let hosts = vec![
        names(&["Realtek HD", "USB Mic"]),       // default host
        names(&["USB Mic", "USB Mic (Backup)"]), // secondary (has SAME name)
    ];
    assert_eq!(
        resolve_over_host_names(&hosts, "USB Mic", "WASAPI"),
        SelectorOutcome::Matched { host: 0, device: 1 },
        "default host must win the tie so the short-circuit returns the \
         same device the full walk would"
    );
}

// ----- Codex P2 (#669 devices.rs:212): usability filter aligns picker + ----
// resolver so a same-name unusable default-host device doesn't hijack
// its usable secondary-host counterpart. The filter itself is applied
// in `enumerate_host_slot_usable` (needs live cpal to test end-to-end);
// what the pure resolver CAN pin is the invariant that makes the fix
// work: given a pre-filtered default-host list that no longer contains
// the unusable device, the usable secondary wins the resolution.

#[test]
fn same_name_secondary_wins_when_default_was_filtered_by_usability() {
    // Regression pin for the #669 devices.rs:212 thread. Simulate
    // `enumerate_host_slot_usable` having already filtered out the
    // default host's "USB Mic" (unusable — 0 input configs). The
    // secondary host's usable "USB Mic" MUST therefore win the
    // exact-match pass.
    //
    // Pre-fix behavior: `resolve_input` enumerated the UNFILTERED
    // default host, exact-short-circuited to its unusable "USB Mic",
    // and `start_capture` then failed in `pick_config` without ever
    // trying the secondary host's usable counterpart.
    let hosts = vec![
        names(&["Realtek HD"]), // default host, "USB Mic" was filtered out
        names(&["USB Mic"]),    // secondary host, usable
    ];
    assert_eq!(
        resolve_over_host_names(&hosts, "USB Mic", "WASAPI"),
        SelectorOutcome::Matched { host: 1, device: 0 },
        "usable secondary MUST be selected when the default host's \
         same-named counterpart was filtered out by the usability check"
    );
}

#[test]
fn secondary_wins_via_substring_when_default_was_filtered_by_usability() {
    // Complementary pin: even a substring match on a filtered-out
    // default-host name must not resurrect the unusable device. With
    // the default host filtered clean, the substring pass sees only
    // the secondary host's candidates.
    let hosts = vec![
        names(&["Realtek HD"]), // default host, "Blue Yeti" filtered out
        names(&["Blue Yeti Classic"]),
    ];
    assert_eq!(
        resolve_over_host_names(&hosts, "Blue Yeti", "WASAPI"),
        SelectorOutcome::Matched { host: 1, device: 0 },
        "substring match must fall to the secondary host when default's \
         same-named unusable variant was filtered out"
    );
}

// ----- Codex P2 (#669 hosts.rs:280): filter preserves native cpal indices ---

#[test]
fn numeric_selector_maps_to_native_cpal_index_when_default_host_has_placeholders() {
    // Regression pin for the sparse-index case. Default host raw cpal
    // enumeration returns [unusable, usable_A, usable_B] and
    // `enumerate_host_slot_usable` preserves positions with empty-
    // string placeholders at slot 0 - so `hosts[0]` is ["", "A", "B"].
    // The picker in `devices::enumerate_all_hosts` publishes usable_A
    // at cpal-native index 1 and usable_B at cpal-native index 2. A
    // user selecting index 1 in the picker MUST open usable_A, not
    // usable_B — pre-fix code compacted the vec (skipping slot 0),
    // shifting usable_A into position 0 and usable_B into position 1,
    // silently opening the wrong device.
    let hosts = vec![vec![
        String::new(),  // cpal index 0: unusable → placeholder
        "A".to_owned(), // cpal index 1: usable_A (picker index 1)
        "B".to_owned(), // cpal index 2: usable_B (picker index 2)
    ]];
    assert_eq!(
        resolve_over_host_names(&hosts, "1", "WASAPI"),
        SelectorOutcome::Matched { host: 0, device: 1 },
        "numeric selector must map to native cpal index (matching \
         picker), not the compacted usable-only position"
    );
    assert_eq!(
        resolve_over_host_names(&hosts, "2", "WASAPI"),
        SelectorOutcome::Matched { host: 0, device: 2 },
        "numeric selector must map to native cpal index for the second \
         usable device too"
    );
}

#[test]
fn numeric_selector_hitting_placeholder_slot_reports_out_of_range() {
    // If a numeric selector lands on an EMPTY placeholder (unusable
    // slot the picker never advertised), the resolver MUST NOT return
    // that device — treat it as out-of-range so capture doesn't fail
    // silently on an unpublished index.
    let hosts = vec![vec![
        String::new(), // cpal index 0: unusable placeholder
        "A".to_owned(),
        "B".to_owned(),
    ]];
    match resolve_over_host_names(&hosts, "0", "WASAPI") {
        SelectorOutcome::NumericOutOfRange { note } => {
            assert!(
                note.contains("index 0 out of range"),
                "must report the specific numeric index: {note}"
            );
            // Count reflects the picker's published (usable) count, not
            // the raw enumeration length. `["", "A", "B"]` has 2 usable.
            assert!(
                note.contains("2 device(s)"),
                "must quote the USABLE device count (matches picker's \
                 published set): {note}"
            );
        }
        other => panic!(
            "numeric selector landing on an unusable placeholder must not \
             resolve, got {other:?}"
        ),
    }
}

// ----- Codex P2 (#669 hosts.rs:424): numeric selectors bypass substring -----

#[test]
fn numeric_selector_wins_over_secondary_substring_containing_digit() {
    // Codex scenario: selector "2" is valid on the default host (3
    // usable devices) AND matches "ASIO Input 2" on a secondary host
    // via substring. Pre-fix the substring pass fired FIRST across all
    // hosts, so "ASIO Input 2" hijacked selector "2" and opened the
    // ASIO device — defeating the "numeric selectors resolve only
    // against the default host" safety rule. Post-fix: parseable
    // numeric selectors skip the substring pass entirely.
    let hosts = vec![
        names(&["Mic A", "Mic B", "Mic C"]), // default host, 3 usable
        names(&["ASIO Input 0", "ASIO Input 1", "ASIO Input 2"]), // digit-bearing names
    ];
    assert_eq!(
        resolve_over_host_names(&hosts, "2", "WASAPI"),
        SelectorOutcome::Matched { host: 0, device: 2 },
        "numeric selector must resolve to default host, not a \
         digit-bearing secondary substring match"
    );
}

#[test]
fn numeric_selector_still_falls_through_when_default_has_no_usable_slot_at_index() {
    // Cross-check for #669 hosts.rs:424 + hosts.rs:280 interaction:
    // when the numeric selector is out of range on the default host
    // AND a secondary device name contains that digit as substring,
    // we STILL return NumericOutOfRange (never fall through to the
    // secondary substring match).
    let hosts = vec![
        names(&["Only Default Mic"]), // default has 1 usable device
        names(&["ASIO Input 2"]),     // secondary contains "2" as substring
    ];
    match resolve_over_host_names(&hosts, "2", "WASAPI") {
        SelectorOutcome::NumericOutOfRange { .. } => {}
        SelectorOutcome::Matched { host, device } => panic!(
            "numeric selector must return NumericOutOfRange, NEVER \
             substring-match a secondary device (got host={host}, \
             device={device})"
        ),
        other => panic!("expected NumericOutOfRange, got {other:?}"),
    }
}

#[test]
fn exact_match_on_device_literally_named_digit_still_wins() {
    // Complementary pin: skipping substring for numeric selectors must
    // NOT skip exact-match. A device LITERALLY named "2" is a
    // legitimate exact hit on any host and should win before the
    // default-host-only numeric branch runs.
    let hosts = vec![
        names(&["Mic A", "Mic B"]), // default: no exact "2"
        names(&["2"]),              // secondary: literal exact "2"
    ];
    assert_eq!(
        resolve_over_host_names(&hosts, "2", "WASAPI"),
        SelectorOutcome::Matched { host: 1, device: 0 },
        "an exact-match on a device literally named '2' must win over \
         the numeric interpretation of the selector"
    );
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

// ----- Codex post-merge P2 (#669 hosts.rs:294): preserve real names ----------
//
// `enumerate_host_slot_usable` now keeps the REAL cpal name for every
// enumerated device — even those `pick_config` cannot open — and
// tracks capture-usability in a parallel `usable: Vec<bool>` on
// [`HostSlot`] / [`HostSnapshot`]. The pure resolver skips unusable
// slots via `mask_names_for_resolver`, and the aggregate error's
// DirectSound-hint suppression consults the FULL cpal name list so a
// visible-but-unopenable device doesn't get the false "only visible
// via Windows DirectSound" claim.

#[test]
fn mask_names_for_resolver_blanks_unusable_slots_but_keeps_usable_ones() {
    // The masking helper is the single seam between the "real names
    // preserved for diagnostics" storage in `HostSlot` and the
    // "empty=skip-in-resolver" shape the pure `resolve_over_host_names`
    // expects. Pin every arm here so a future refactor cannot silently
    // drift the two conventions apart.
    let names = vec![
        "Realtek HD".to_owned(),
        "USB Mic Unusable".to_owned(),
        "USB Mic Working".to_owned(),
        "".to_owned(), // already-blank slot
    ];
    let usable = vec![true, false, true, false];
    let masked = mask_names_for_resolver(&names, &usable);
    assert_eq!(masked[0], "Realtek HD", "usable slot passes through");
    assert_eq!(
        masked[1], "",
        "unusable slot is blanked so the resolver skips it"
    );
    assert_eq!(masked[2], "USB Mic Working", "second usable slot passes");
    assert_eq!(
        masked[3], "",
        "already-blank slot stays blank (usable=false regardless)"
    );
}

#[test]
fn resolver_skips_unusable_slot_but_diagnostic_still_shows_the_real_name() {
    // The pure resolver: an empty-masked (unusable) slot never
    // matches by name. "Blue Yeti" is not a substring of "Realtek HD"
    // and vice versa, so the substring pass finds nothing after the
    // masked slot at position 0.
    let hosts = vec![vec![String::new(), "Realtek HD".to_owned()]];
    assert_eq!(
        resolve_over_host_names(&hosts, "Blue Yeti", "WASAPI"),
        SelectorOutcome::NotFound,
        "masked (unusable) slot must not match by name"
    );
}

#[test]
fn selector_matches_any_cpal_name_suppresses_directsound_hint_for_unusable_device() {
    // Regression pin for #669 post-merge hosts.rs:294. The DirectSound
    // hint MUST be suppressed when cpal already enumerated a name
    // matching the selector — even if the device is capture-unusable.
    // Otherwise a visible-but-unopenable cpal device (e.g. a Blue
    // Yeti whose `supported_input_configs` transiently failed) gets
    // the false "only visible via Windows DirectSound" remediation.
    //
    // The pure predicate is cross-platform testable — this test
    // FAILS on pre-fix behavior (predicate returns false → hint
    // check runs → false-positive) on every OS.
    let cpal_names = ["Blue Yeti", "Realtek HD"];
    assert!(
        selector_matches_any_cpal_name("Blue Yeti", &cpal_names),
        "exact match must trigger suppression"
    );
    // Bidirectional substring: saved "Blue Yeti Classic" should
    // match an enumerated "Blue Yeti" and suppress the hint.
    assert!(
        selector_matches_any_cpal_name("Blue Yeti Classic", &cpal_names),
        "bidirectional substring match must trigger suppression"
    );
    assert!(
        !selector_matches_any_cpal_name("__completely_unrelated__", &cpal_names),
        "unrelated names must NOT suppress"
    );
    // Empty entries (masked placeholders) are IGNORED — they're not
    // real cpal names and must not spuriously suppress the hint.
    assert!(
        !selector_matches_any_cpal_name("Blue Yeti", &["", ""]),
        "empty (masked) entries must not count as cpal enumeration"
    );
}

#[test]
fn build_not_found_error_suppresses_directsound_hint_when_cpal_saw_the_name() {
    // End-to-end pin for the suppression pathway. Build a snapshot
    // with a REAL name preserved + usable=false (mimicking the
    // "enumerated but capture-unusable" case). The aggregate error
    // MUST NOT include the DirectSound remediation for that
    // selector — the hint suppressor consults device_names.
    let host_snap = HostSnapshot {
        host_id: cpal::default_host().id(),
        host_label: "WASAPI",
        device_names: vec!["Blue Yeti".to_owned()],
        usable: vec![false],
        enumeration_error: None,
    };
    let err = build_not_found_error("Blue Yeti", &[host_snap], None);
    let msg = err.to_string();
    assert!(
        !msg.contains("only visible via Windows DirectSound"),
        "DirectSound hint must be suppressed when cpal enumerated the \
         name (even if unusable): {msg}"
    );
}

// ----- Codex post-merge P2 (#669 hosts.rs:200): failed hosts not searched ---
//
// A failed-host placeholder (constructor OR input_devices() failed)
// MUST NOT be counted as "successfully searched" in the aggregate
// error. Its enumeration_error is carried into the aggregate as a
// separate `enumeration failures: ...` clause so an outage is
// diagnosable rather than masquerading as a name miss.

#[test]
fn not_found_error_excludes_failed_hosts_from_the_searched_count() {
    // Simulate: default host WASAPI failed; secondary ASIO succeeded
    // with 2 usable devices. The aggregate error MUST NOT quote
    // "searched X across 2 host(s): WASAPI, ASIO" — WASAPI was
    // never searched.
    let snaps = vec![
        failed_snapshot(
            "WASAPI",
            "host WASAPI: input_devices() failed (permission denied)",
        ),
        snapshot("ASIO", &["Studio Mic A", "Studio Mic B"]),
    ];
    let err = build_not_found_error("Ghost", &snaps, None);
    let msg = err.to_string();
    // Search stats reflect ONLY the successful host.
    assert!(
        msg.contains("across 1 host(s)"),
        "failed host must NOT count toward the search stats: {msg}"
    );
    assert!(
        msg.contains("searched 2 device(s)"),
        "device count must reflect only successfully-searched hosts: {msg}"
    );
    assert!(
        msg.contains("ASIO"),
        "successful host label must appear: {msg}"
    );
    // Failure appears in its own clause — separable from the search stats.
    assert!(
        msg.contains("enumeration failures:"),
        "failed host's error must be carried in a distinct 'enumeration \
         failures:' clause: {msg}"
    );
    assert!(
        msg.contains("permission denied"),
        "underlying failure detail must be preserved: {msg}"
    );
}

#[test]
fn should_push_secondary_slot_retains_failed_hosts_for_diagnostics() {
    // Codex P2 (#674 hosts.rs:222) regression pin. Pre-fix code
    // guarded push on `slot.enumeration_error.is_none()`, so a
    // failed secondary host was silently dropped and its
    // enumeration_error never reached the aggregate error.
    // Post-fix: ALWAYS push, so `build_not_found_error` sees the
    // failure via the snapshot's `enumeration_error` field.
    //
    // We fabricate two synthetic slots via `HostSlot`'s private
    // constructor path — `should_push_secondary_slot` is pure, so
    // exercising it with a real cpal::Device isn't required. But we
    // DO need at least one to build a HostSlot; use the default cpal
    // host's default input device (may be absent on headless boxes —
    // gate via `Option`).
    let default_host = cpal::default_host();
    let host_id = default_host.id();
    let host_label = host_id.name();

    let succeeded = HostSlot {
        host_id,
        host_label,
        devices: Vec::new(),
        names: Vec::new(),
        usable: Vec::new(),
        enumeration_error: None,
    };
    let failed = HostSlot {
        host_id,
        host_label,
        devices: Vec::new(),
        names: Vec::new(),
        usable: Vec::new(),
        enumeration_error: Some(
            "host ASIO: input_devices() failed (device in use by another application)".to_owned(),
        ),
    };

    assert!(
        should_push_secondary_slot(&succeeded),
        "successful slots must be pushed (they may carry devices to search)"
    );
    assert!(
        should_push_secondary_slot(&failed),
        "FAILED slots must ALSO be pushed so their enumeration_error \
         reaches the aggregate error's 'enumeration failures:' clause \
         (Codex P2 #674 hosts.rs:222). Pre-fix behavior dropped them, \
         silently eating the diagnostic."
    );
}

#[test]
fn not_found_error_reports_failed_secondary_hosts_when_default_succeeded() {
    // Codex P2 (#674 hosts.rs:222): when the default host enumerates
    // successfully but a SECONDARY host fails (transient ASIO / JACK /
    // Pulse outage), the failed slot MUST be reported in the aggregate
    // `enumeration failures:` clause. Pre-fix the failed secondary
    // slot was dropped entirely, silently eating the diagnostic and
    // making the outage look identical to a plain name miss.
    let snaps = vec![
        snapshot("WASAPI", &["Realtek HD", "USB Mic"]), // default succeeded
        failed_snapshot(
            "ASIO",
            "host ASIO: input_devices() failed (device in use by another application)",
        ),
    ];
    let err = build_not_found_error("Ghost", &snaps, None);
    let msg = err.to_string();
    // Failed host does NOT count as searched, but the searched stats
    // still reflect the default's usable devices.
    assert!(
        msg.contains("searched 2 device(s) across 1 host(s): WASAPI"),
        "searched stats must reflect only successful hosts: {msg}"
    );
    // The failed-host diagnostic MUST appear in the aggregate error
    // so an outage is separable from a name miss.
    assert!(
        msg.contains("enumeration failures:"),
        "failed secondary host's error MUST be surfaced in the \
         aggregate error: {msg}"
    );
    assert!(
        msg.contains("host ASIO"),
        "failed host label must survive into the aggregate error: {msg}"
    );
    assert!(
        msg.contains("device in use by another application"),
        "underlying secondary-host failure detail must be preserved: {msg}"
    );
}

#[test]
fn not_found_error_omits_enumeration_failures_clause_when_no_failures() {
    // Complementary pin: no failed hosts → no `; enumeration
    // failures:` clause. Absence of noise for the healthy case.
    let snaps = vec![snapshot("WASAPI", &["Mic A"])];
    let err = build_not_found_error("Ghost", &snaps, None);
    let msg = err.to_string();
    assert!(
        !msg.contains("enumeration failures:"),
        "no failure clause when every host succeeded: {msg}"
    );
}

// ----- Codex post-merge P2 (#669 devices.rs:271): pick-config strict filter -
//
// `device_supports_rust_capture` is the resolver's pure "would
// pick_config open this device?" predicate. Live cpal-integration is
// exercised through the resolver + capture paths; the unit-level pin
// is that the helper only accepts devices with at least one usable
// F32/I16/I32 supported config (rejecting `default_input_config`-only
// devices and non-F32/I16/I32 formats).
//
// We can't easily fabricate a `cpal::Device` in a unit test, so the
// pin below asserts the helper's DOCUMENTED contract at the compile-
// / API-boundary level. The actual behaviour is exercised by any
// resolver / picker call on live hardware — a device that would
// otherwise sneak in via `default_input_config` fallback is now
// excluded (see the enumerate_host_slot_usable + append_host_devices
// call sites).

#[test]
fn device_supports_rust_capture_is_visible_to_the_devices_picker() {
    // Cross-crate symbol check: the picker (`devices::append_host_devices`
    // under rust_capture_strict) MUST call the same helper the resolver
    // uses, so both stay in sync. This test compiles iff the helper is
    // reachable via its `pub(crate)` path from tests, which mirrors the
    // path devices.rs uses.
    let _: fn(&cpal::Device) -> bool = super::device_supports_rust_capture;
}

// ----- Codex P2 (#674 devices.rs:600): exercise the strict-filter contract --

#[test]
fn sample_config_is_rust_openable_accepts_f32_i16_i32_with_channels() {
    // Positive cases: the three sample formats `pick_config` handles,
    // each with a non-zero channel count. Every arm here MUST be true
    // — otherwise the picker's strict filter would over-prune valid
    // microphones (dropping usable devices from the Settings picker
    // and silently forcing the user to a fallback).
    assert!(sample_config_is_rust_openable(cpal::SampleFormat::F32, 1));
    assert!(sample_config_is_rust_openable(cpal::SampleFormat::F32, 2));
    assert!(sample_config_is_rust_openable(cpal::SampleFormat::I16, 1));
    assert!(sample_config_is_rust_openable(cpal::SampleFormat::I16, 8));
    assert!(sample_config_is_rust_openable(cpal::SampleFormat::I32, 1));
}

#[test]
fn sample_config_is_rust_openable_rejects_non_pick_config_formats() {
    // Negative cases: every sample format `pick_config` cannot open
    // (see `capture.rs::pick_config` — the `_` arm ignores everything
    // except F32/I16/I32). A regression that INVERTED the predicate
    // (or dropped the format check entirely) would light these up.
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::U8, 1));
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::U16, 1));
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::U32, 1));
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::I8, 1));
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::F64, 1));
    // 8-bit unsigned + high channel count STILL rejected: format wins.
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::U16, 32));
}

#[test]
fn sample_config_is_rust_openable_rejects_zero_channel_configs() {
    // Zero-channel configs never open — mirror
    // `probe_device_config`'s `channels > 0` filter so the picker
    // agrees with the resolver on which mics are selectable.
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::F32, 0));
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::I16, 0));
    assert!(!sample_config_is_rust_openable(cpal::SampleFormat::I32, 0));
}
