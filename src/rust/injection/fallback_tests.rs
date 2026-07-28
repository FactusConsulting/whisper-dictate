//! Narrow unit tests for the [`super::fallback`] runtime-fallback API
//! introduced by PR #613.
//!
//! Companion to the two inline `#[cfg(test)]` modules already inside
//! `fallback.rs` (`tests`, `runtime_fallback_tests`) -- those cover the
//! chain-selection surface and the `is_safe_to_try_next_helper`
//! text-matcher end-to-end. This sibling file pins the specific
//! symbol-level contracts the runtime dispatcher relies on
//! (`HelperError::opaque`, `HelperError::partial`,
//! `available_helpers`, and the `is_safe_to_try_next_helper` gate),
//! so the regression-test-discipline scanner can see the new public
//! API surface exercised by name from a dedicated `fallback_tests.rs`.

use super::fallback::{available_helpers, is_safe_to_try_next_helper, HelperError, LinuxSession};
use std::path::PathBuf;

#[test]
fn helper_error_opaque_defaults_partial_false() {
    // Subprocess helpers (kwtype/wtype/xdotool/dotool as a single
    // opaque child) cannot see partial progress; the constructor MUST
    // leave `partial` clear so the dispatcher's text-match safety gate
    // (`is_safe_to_try_next_helper`) is the sole authority for the
    // "safe to retry" decision.
    let err = HelperError::opaque(anyhow::anyhow!("kwtype exit 1"));
    assert!(!err.partial, "opaque(_) must not set the partial flag");
    assert!(
        !err.known_no_progress,
        "opaque(_) must leave known_no_progress clear -- 'opaque' means \
         we CANNOT tell whether any keys landed; only none_landed(_) \
         asserts positive proof"
    );
    assert!(err.err.to_string().contains("kwtype exit 1"));
}

#[test]
fn helper_error_partial_sets_partial_true() {
    // The evdev-driven ydotool path can observe partial progress; the
    // constructor MUST stamp `partial: true` so the dispatcher refuses
    // to try the next helper (retrying would double-type the injected
    // prefix into the user's document).
    let err = HelperError::partial(anyhow::anyhow!("ydotool died mid-word"));
    assert!(err.partial, "partial(_) must set the partial flag");
    assert!(err.err.to_string().contains("ydotool died mid-word"));
}

#[test]
fn helper_error_none_landed_proves_no_progress_without_partial() {
    // Codex P2 #636 dispatcher.rs:708 -- the ydotool `sent == 0` path
    // is a POSITIVE PROOF that nothing reached the compositor. The
    // constructor MUST leave `partial` clear (unlike `partial(...)`)
    // AND set `known_no_progress` so the dispatcher's idx>0
    // opaque-failure branch does not stamp `partial=true` on this
    // outcome and lose the transcript to the Python outer-fallback
    // suppression.
    let err = HelperError::none_landed(anyhow::anyhow!("ydotool: broken pipe before first op"));
    assert!(!err.partial, "none_landed(_) must not set the partial flag");
    assert!(
        err.known_no_progress,
        "none_landed(_) must set known_no_progress so the dispatcher skips \
         the idx>0 partial stamp (Codex P2 #636 dispatcher.rs:708)"
    );
    assert!(err.err.to_string().contains("broken pipe"));
}

#[test]
fn is_safe_to_try_next_helper_recognises_known_startup_failures_and_rejects_novel_ones() {
    // Narrow companion to `runtime_fallback_tests::*` in fallback.rs.
    // Pins both directions of the safety gate at the symbol level so a
    // reader landing here from the scanner failure sees the intent.
    assert!(is_safe_to_try_next_helper(
        "wtype: compositor does not support the protocol"
    ));
    assert!(is_safe_to_try_next_helper("No such file or directory"));
    // Anything not on the known-startup-failure list must FAIL the
    // gate -- an unrecognised error might mean the helper typed part
    // of the transcript before dying, in which case retrying would
    // corrupt the document.
    assert!(!is_safe_to_try_next_helper("killed by signal 9"));
    assert!(!is_safe_to_try_next_helper("some entirely new error"));
}

#[test]
fn available_helpers_returns_installed_subset_in_chain_order() {
    // The whole point of the new `available_helpers` public: give the
    // runtime dispatcher the FULL ordered list (not just the first)
    // so it can walk the chain. Pin that: the returned Vec must be
    // non-empty when at least one helper is installed AND must reflect
    // the chain order.
    let installed = ["wtype", "ydotool"];
    let locator = |name: &str| {
        installed
            .contains(&name)
            .then(|| PathBuf::from(format!("/usr/bin/{name}")))
    };
    let helpers = available_helpers(LinuxSession::OtherWayland, locator, false);
    assert!(!helpers.is_empty(), "must return the installed helpers");
    // OtherWayland chain is [wtype, dotool, ydotool]; with only
    // wtype+ydotool installed the result must preserve that order.
    assert_eq!(helpers, vec!["wtype", "ydotool"]);
}
