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
