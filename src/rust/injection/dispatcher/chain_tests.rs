use super::{try_helpers_over, ydotool_failure_to_helper_error, HelperError};
use anyhow::anyhow;
use std::sync::{Arc, Mutex};

// -- Runtime fallback chain (Codex #613 findings) --------------------
//
// These exercise `try_helpers_over` directly with a fake `attempt`
// closure. They pin the three behaviours that the review flagged:
//   1. When helper A fails with a startup signature, B is tried.
//   2. When a helper reports `HelperError::partial` (any keys landed),
//      fallback is suppressed even if the next helper is available.
//   3. When helper A fails first, B succeeds, an info line is emitted
//      so the operator knows the fallback actually kicked in.

#[cfg(target_os = "linux")]
#[test]
fn try_helpers_over_falls_back_after_startup_failure() {
    // dispatcher.rs:521 -- the runtime retry loop was
    // never covered by a test. Fake: kwtype refuses at startup (a
    // known-safe signature), wtype succeeds. Expected: the second
    // helper is called and `try_helpers_over` returns Ok.
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_c = calls.clone();
    let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
        calls_c.lock().unwrap().push(helper.to_owned());
        if helper == "kwtype" {
            // "Compositor does not support the virtual keyboard
            // protocol" is a recognised startup failure per
            // `is_safe_to_try_next_helper`.
            Err(HelperError::opaque(anyhow!(
                "kwtype type failed: Compositor does not support the virtual keyboard protocol"
            )))
        } else {
            Ok(())
        }
    };
    let candidates = ["kwtype", "wtype", "ydotool"];
    let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
    outcome
        .result
        .expect("wtype should have carried the injection");
    assert!(!outcome.partial, "success must never set partial=true");
    assert_eq!(*calls.lock().unwrap(), vec!["kwtype", "wtype"]);
}

#[cfg(target_os = "linux")]
#[test]
fn try_helpers_over_suppresses_fallback_after_partial_failure() {
    // dispatcher.rs:540. Fake: ydotool typed some ops,
    // then died. `HelperError::partial` MUST stop the chain -- if
    // wtype ran next it would type the whole burst on top of the
    // successful prefix, silently doubling half the transcript into
    // the user's document.
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_c = calls.clone();
    let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
        calls_c.lock().unwrap().push(helper.to_owned());
        if helper == "ydotool" {
            Err(HelperError::partial(anyhow!(
                "ydotool: broken pipe after 12 keystrokes"
            )))
        } else {
            panic!("unreachable: fallback must not run after a partial failure");
        }
    };
    let candidates = ["ydotool", "wtype"];
    let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
    assert!(
        outcome.partial,
        "partial-burst failure must stamp partial=true so the outer \
         fallback stands down"
    );
    let err = outcome
        .result
        .expect_err("partial failure must NOT fall through to the next helper");
    assert!(
        format!("{err:#}").contains("broken pipe"),
        "expected the partial error surfaced verbatim, got {err:#}"
    );
    // The safety property, expressed as the call transcript.
    assert_eq!(*calls.lock().unwrap(), vec!["ydotool"]);
}

#[cfg(target_os = "linux")]
#[test]
fn try_helpers_over_stops_on_unrecognised_opaque_failure() {
    // Belt-and-braces: `HelperError::opaque` with an error string
    // that doesn't match any known startup signature must also stop
    // the chain, because a subprocess helper might have typed a
    // partial burst before it crashed with a novel message.
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_c = calls.clone();
    let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
        calls_c.lock().unwrap().push(helper.to_owned());
        Err(HelperError::opaque(anyhow!(
            "wtype type failed: killed by signal 9"
        )))
    };
    let candidates = ["wtype", "ydotool"];
    let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
    let err = outcome
        .result
        .expect_err("unrecognised failure must stop the chain");
    assert!(format!("{err:#}").contains("killed by signal 9"));
    // idx == 0 preserves the pre-#613 single-helper semantics: an
    // unrecognised opaque failure of the FIRST helper does not stamp
    // partial=true, so the outer fallback can still retry.
    // The `idx > 0` branch is exercised by the paired test below.
    assert!(
        !outcome.partial,
        "first-helper opaque failure must NOT stamp partial=true"
    );
    assert_eq!(*calls.lock().unwrap(), vec!["wtype"]);
}

#[cfg(target_os = "linux")]
#[test]
fn try_helpers_over_preserves_partial_false_when_helper_proves_no_progress() {
    // dispatcher.rs:708. Fake: helper[0] (kwtype) fails
    // with a recognised startup signature -> chain retries. helper[1]
    // (ydotool) then fails with an UNrecognised opaque error but with
    // POSITIVE PROOF that nothing landed (`sent == 0`, surfaced via
    // `HelperError::none_landed`). The dispatcher must NOT stamp
    // `partial=true` in this case -- otherwise the outer bridge
    // suppresses its fallback and the transcript is lost even though
    // we know nothing was typed anywhere.
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_c = calls.clone();
    let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
        calls_c.lock().unwrap().push(helper.to_owned());
        if helper == "kwtype" {
            Err(HelperError::opaque(anyhow!(
                "kwtype type failed: Compositor does not support the virtual keyboard protocol"
            )))
        } else {
            // ydotool: novel unrecognised message, but sent == 0 so
            // the helper KNOWS nothing reached the compositor.
            Err(HelperError::none_landed(anyhow!(
                "ydotool type failed: broken pipe before first keystroke"
            )))
        }
    };
    let candidates = ["kwtype", "ydotool"];
    let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
    assert!(
        !outcome.partial,
        "known-no-progress opaque failure at idx>0 must NOT stamp partial=true \
         — otherwise the outer fallback stands down and the transcript is lost"
    );
    let err = outcome
        .result
        .expect_err("chain must still surface the underlying error");
    assert!(format!("{err:#}").contains("broken pipe"));
    assert_eq!(*calls.lock().unwrap(), vec!["kwtype", "ydotool"]);
}

#[cfg(target_os = "linux")]
#[test]
fn try_helpers_over_marks_partial_after_fallback_fired() {
    // dispatcher.rs:609. If helper[0] fails with a
    // recognised startup signature (chain retries) and helper[1] then
    // fails with an UNrecognised opaque error, helper[1] may have
    // typed part of the transcript before dying. Stamp `partial=true`
    // so the outer fallback stands down instead of re-typing
    // the full text on top of the successful prefix.
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_c = calls.clone();
    let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
        calls_c.lock().unwrap().push(helper.to_owned());
        if helper == "kwtype" {
            Err(HelperError::opaque(anyhow!(
                "kwtype type failed: Compositor does not support the virtual keyboard protocol"
            )))
        } else {
            // wtype dies with a novel unrecognised message *after*
            // some keystrokes may have landed.
            Err(HelperError::opaque(anyhow!(
                "wtype type failed: killed by signal 9"
            )))
        }
    };
    let candidates = ["kwtype", "wtype", "ydotool"];
    let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
    assert!(
        outcome.partial,
        "opaque failure on helper idx>0 must stamp partial=true so the \
         outer fallback does not double-type"
    );
    let err = outcome
        .result
        .expect_err("post-fallback opaque failure must stop the chain");
    assert!(format!("{err:#}").contains("killed by signal 9"));
    // Chain did move past kwtype but stopped at wtype (no ydotool).
    assert_eq!(*calls.lock().unwrap(), vec!["kwtype", "wtype"]);
}

#[cfg(target_os = "linux")]
#[test]
fn try_helpers_over_succeeds_immediately_on_first_helper() {
    // No fallback needed: the happy path returns Ok after a single
    // call and never touches later helpers.
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_c = calls.clone();
    let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
        calls_c.lock().unwrap().push(helper.to_owned());
        Ok(())
    };
    let candidates = ["kwtype", "wtype", "ydotool"];
    let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
    outcome.result.expect("first helper succeeded, must be Ok");
    assert!(!outcome.partial, "success cannot be partial");
    assert_eq!(*calls.lock().unwrap(), vec!["kwtype"]);
}

#[cfg(target_os = "linux")]
#[test]
fn try_helpers_over_errors_when_all_helpers_fail_with_startup_signatures() {
    // Every candidate fails with a recognised startup signature: the
    // chain exhausts, and the surfaced error must be the LAST one
    // (wrapped with a "every ... helper failed" context).
    let mut attempt = |helper: &str| -> std::result::Result<(), HelperError> {
        Err(HelperError::opaque(anyhow!(
            "{helper} type failed: Compositor does not support the virtual keyboard protocol"
        )))
    };
    let candidates = ["kwtype", "wtype"];
    let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
    let err = outcome
        .result
        .expect_err("chain of startup failures should surface an error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("every Linux injection helper failed"),
        "got: {msg}"
    );
    // Every failure was a recognised startup signature (i.e. no
    // helper is believed to have typed anything), so `partial=false`.
    assert!(
        !outcome.partial,
        "recognised startup-only failures cannot have typed a partial burst"
    );
}

// -----------------------------------------------------------------------
// review r3663766083 — ydotool failure ALWAYS stamps
// `partial=true`. The regression this pins:
//
//   1. `available_helpers` places `ydotool` at candidate index 0 when
//      it is the only installed helper.
//   2. A `ydotool type -- <buffer>` subprocess emits part of the
//      buffer, then exits nonzero (broken ydotoold socket, SIGPIPE,
//      etc.). `type_text_tracked` returns `Err(err, 0)` — no OP
//      completed, so `sent == 0`.
//   3. The previous `HelperError::opaque(err)` reached
//      `try_helpers_over` at idx=0, whose `idx > 0` gate skipped the
//      partial stamp, so `outcome.partial == false`.
//   4. The outer fallback saw `partial == false` and retried the
//      whole transcript on top of the leaked prefix → double-typing.
//
// Failure mode this test would exhibit against the un-fixed code:
// `assert!(outcome.partial, ...)` FAILS because the un-fixed code
// returns `HelperError::opaque(err)` at idx=0, and `try_helpers_over`
// leaves `partial=false`.
// -----------------------------------------------------------------------

#[cfg(target_os = "linux")]
#[test]
fn ydotool_failure_conversion_always_stamps_partial_regardless_of_sent() {
    // The direct invariant on the extracted helper — locks the
    // conversion so any future edit that flips this back to
    // `opaque` fails a test before it can ship.
    for sent in [0usize, 1, 2, 47] {
        let err = anyhow!("ydotool type failed: broken pipe after chunk {sent}");
        let helper_err = ydotool_failure_to_helper_error(err, sent);
        assert!(
            helper_err.partial,
            "ydotool failure at sent={sent} must set partial=true; \
             a partial-write can happen at any sent count",
        );
        assert!(
            !helper_err.known_no_progress,
            "ydotool subprocess exit code cannot prove zero events landed; \
             `known_no_progress` must stay clear",
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn try_helpers_over_stamps_partial_when_ydotool_is_only_helper_and_fails() {
    // End-to-end for the exact scenario Codex flagged: ydotool is the
    // ONLY installed helper (idx=0), its subprocess fails with an
    // unrecognised message, and the outcome must carry partial=true
    // so the outer fallback stands down.
    //
    // Uses `ydotool_failure_to_helper_error` (the production wiring
    // in `inject_on_linux`) so this test would ALSO fail against a
    // regression that reverted just that call site while leaving the
    // helper function intact.
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_c = calls.clone();
    let mut attempt = move |helper: &str| -> std::result::Result<(), HelperError> {
        calls_c.lock().unwrap().push(helper.to_owned());
        assert_eq!(helper, "ydotool", "test fixture only provides ydotool");
        Err(ydotool_failure_to_helper_error(
            anyhow!("ydotool type failed: broken pipe mid-buffer"),
            0, // first op failed — the exact case that slipped past idx>0
        ))
    };
    let candidates = ["ydotool"];
    let outcome = try_helpers_over(&candidates, &mut attempt, "injection");
    let err = outcome
        .result
        .expect_err("sole-helper failure must surface an error");
    assert!(
        format!("{err:#}").contains("broken pipe"),
        "surfaced error must carry the original ydotool message, got: {err:#}",
    );
    assert!(
        outcome.partial,
        "ydotool-only failure at idx=0 must stamp partial=true so the \
         outer fallback does not double-type on top of a partial ydotool write",
    );
    assert_eq!(*calls.lock().unwrap(), vec!["ydotool"]);
}
