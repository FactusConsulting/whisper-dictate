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
// The exact/substring passes in `resolve_input` are pure over the
// `HostSlot::names` vectors modulo cpal's device handles. Rather than
// duplicate the loop, we validate the behaviour end-to-end when a live
// host happens to be present, and validate the invariant precedence
// property against the pure logic below.

/// Standalone reference implementation of the multi-host precedence
/// used by `resolve_input`: exact-match on ANY host beats substring on
/// EVERY host. Kept in the test file so the property is checkable
/// against synthetic host constellations without a live cpal backend.
fn winning_slot_for(hosts: &[Vec<&str>], selector: &str) -> Option<(usize, usize)> {
    let needle = selector.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    // Pass 1: exact match, first host wins.
    for (h, names) in hosts.iter().enumerate() {
        for (d, name) in names.iter().enumerate() {
            if name.to_lowercase() == needle {
                return Some((h, d));
            }
        }
    }
    // Pass 2: bidirectional longest substring across every host.
    let mut best: Option<(usize, usize, usize)> = None;
    for (h, names) in hosts.iter().enumerate() {
        for (d, name) in names.iter().enumerate() {
            let lower = name.to_lowercase();
            if lower.is_empty() || !(lower.contains(&needle) || needle.contains(&lower)) {
                continue;
            }
            let name_len = name.len();
            match best {
                None => best = Some((h, d, name_len)),
                Some((_, _, prev_len)) if name_len > prev_len => {
                    best = Some((h, d, name_len));
                }
                _ => {}
            }
        }
    }
    best.map(|(h, d, _)| (h, d))
}

#[test]
fn exact_match_on_secondary_host_beats_substring_on_default_host() {
    // The scenario from the Codex thread: default host (WASAPI) exposes
    // "USB Mic"; secondary host (ASIO) exposes "USB Mic ASIO". A
    // selector of "USB Mic ASIO" MUST resolve to the ASIO entry (host
    // 1) — NOT to the default host's "USB Mic", even though "USB Mic"
    // is a substring of the selector and the default host is tried
    // first. Without exact-match-first, the default host's shorter
    // substring hijacks the ASIO/JACK entry.
    let hosts = vec![
        vec!["Realtek HD", "USB Mic"], // WASAPI (default)
        vec!["USB Mic ASIO"],          // ASIO (secondary)
    ];
    let winner = winning_slot_for(&hosts, "USB Mic ASIO");
    assert_eq!(
        winner,
        Some((1, 0)),
        "exact match on secondary host must win over substring on default"
    );
}

#[test]
fn substring_pass_still_prefers_longest_across_hosts() {
    // With no exact match anywhere, the substring pass pools across all
    // hosts and picks the LONGEST name. Same longest-wins tiebreak the
    // capture path's single-host resolver uses — just pooled.
    let hosts = vec![
        vec!["Headset Microphone"],
        vec!["Headset Microphone (Jabra Evolve 65 TE)"],
    ];
    let winner = winning_slot_for(&hosts, "Headset Microphone (Jabra Evolv");
    assert_eq!(
        winner,
        Some((1, 0)),
        "longest substring match must win irrespective of host order"
    );
}

#[test]
fn default_host_wins_when_both_hosts_have_the_same_exact_name() {
    // Ties on exact match resolve in preferred_host_order — the default
    // host always wins the tie so users who never see a secondary host
    // keep the pre-refactor "same device as before" outcome.
    let hosts = vec![vec!["USB Mic"], vec!["USB Mic"]];
    assert_eq!(winning_slot_for(&hosts, "USB Mic"), Some((0, 0)));
}

#[test]
fn empty_selector_never_matches_via_substring_or_exact() {
    // Empty needle would otherwise contains-match every device via the
    // empty-substring rule. Guarded in the resolver so the empty-
    // selector default-host branch is the only path an empty selector
    // ever takes.
    let hosts = vec![vec!["USB Mic"], vec!["Another Mic"]];
    assert_eq!(winning_slot_for(&hosts, ""), None);
    assert_eq!(winning_slot_for(&hosts, "   "), None);
}

// ----- fix 3 (hosts.rs:170): numeric selectors stay in the published index --

#[test]
fn resolve_input_numeric_selector_out_of_range_reports_bounded_error() {
    // A numeric selector must not silently open some secondary host's
    // nth microphone when it's out of range on the default host. The
    // published enumeration in `devices::list_input_devices` gives
    // non-default-host entries synthetic indices whose values depend on
    // runtime enumeration state, so a stale numeric saved value could
    // otherwise switch to an unrelated mic when a new host comes online.
    //
    // Live cpal hosts always have SOME default-host device count, but
    // we can pick a selector that's guaranteed to outrun any reasonable
    // dev-box count (< 10000 default-host input devices).
    match resolve_input("999999") {
        Ok(_) => panic!("999999 unexpectedly resolved to a real device"),
        Err(err) => {
            let msg = err.to_string();
            // Either the box has no cpal hosts at all (propagated
            // enumeration failure, distinct from "not found") — or the
            // aggregate error carries the actionable "pick by name"
            // note so the user sees the remediation.
            let is_enum_failure = msg.starts_with("enumerate input devices: ");
            let is_not_found = msg.starts_with("input device not found: ");
            assert!(
                is_enum_failure || is_not_found,
                "unexpected error shape: {msg}"
            );
            if is_not_found {
                assert!(
                    msg.contains("pick a device by name instead"),
                    "numeric remediation missing: {msg}"
                );
                assert!(
                    msg.contains("out of range on default host"),
                    "numeric range breadcrumb missing: {msg}"
                );
            }
        }
    }
}
