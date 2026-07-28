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
