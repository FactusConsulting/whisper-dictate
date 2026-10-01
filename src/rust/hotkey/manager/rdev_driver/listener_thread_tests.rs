//! Guard the actual native listener's readiness and liveness ordering.

/// #668  3664983439 — the liveness atomic MUST be
/// flipped before any synchronous `diag::log!` call after
/// `rdev::listen` returns. Ordering matters: if the diagnostic sink
/// stalls (blocked AppData I/O on Windows), a boot-self-test polling
/// `is_listener_alive()` during the hold window would still read
/// `true` and misreport PASS on the exact dead-hook condition this
/// signal exists to detect.
///
/// Purely dynamic testing this ordering requires a stallable diag
/// sink, which the shipping code has no seam for. A source-level scan
/// is enough: it fails on the exact pre-fix layout (log first, then
/// store) and passes on the fix (store first, then log). Any future
/// refactor that reintroduces the ordering bug also fails this test.
#[test]
fn rdev_listener_flips_alive_atomic_before_diag_log_after_listen_returns() {
    use std::fs;
    let src = fs::read_to_string("src/rust/hotkey/manager/rdev_driver/listener_thread.rs")
        .or_else(|_| fs::read_to_string("hotkey/manager/rdev_driver/listener_thread.rs"))
        .expect("listener_thread.rs must be readable from the crate root");
    // Find the `rdev::listen(cb)` call site.
    let listen_call = src
        .find("rdev::listen(cb)")
        .expect("rdev::listen(cb) call must exist in the listener thread body");
    // Look at the ~800 chars immediately after — well past the two
    // return branches but short enough that we do not stray into the
    // heartbeat/other listeners.
    let window_end = (listen_call + 1200).min(src.len());
    let after = &src[listen_call..window_end];
    let store_idx = after.find("listener_alive_for_thread.store(false").expect(
        "the listener body must clear the alive atomic after \
             rdev::listen returns (#668  3664983439)",
    );
    let first_log_idx = after
        .find("crate::diag::log!")
        .expect("the listener body must log after rdev::listen returns");
    assert!(
        store_idx < first_log_idx,
        "listener_alive_for_thread.store(false) must come BEFORE the \
         first `crate::diag::log!` after rdev::listen returns. Ordering \
         matters: a stalled diag sink (blocked AppData I/O on Windows) \
         between the log call and the atomic store would let a \
         boot-self-test polling `is_listener_alive()` still read `true` \
         on a dead hook. #668  3664983439."
    );
}

/// Structural companion: the runtime tests above drive the extracted
/// halves; this one pins that the PRODUCTION listener body is wired to
/// them. A regression that went back to sending `Started` unconditionally
/// (never consulting the writer) would leave the runtime tests green
/// while shipping the silent-dead-queue behaviour again.
#[test]
fn production_listener_primes_the_writer_before_announcing_ready() {
    let body = crate::diag_tests::scan_fn_body(
        "src/rust/hotkey/manager/rdev_driver/listener_thread.rs",
        "fn run_native_listener<F, R>(",
    );
    let handshake = body
        .code
        .find("listener_readiness_handshake")
        .expect("the listener must run the readiness handshake helper");
    let writer = body
        .code
        .find("async_writer_result")
        .expect("the listener must consult the writer's spawn outcome");
    assert!(
        writer < handshake,
        "the writer outcome must be resolved BEFORE the handshake sends          Started, otherwise a dead writer is discovered only after `spawn`          already returned Ok. Offending function body:
{}",
        body.raw
    );
    assert!(
        body.code.contains("callback_diagnostics_enabled()"),
        "the production handshake must be handed \
         `crate::diag::callback_diagnostics_enabled()`. Hardcoding `true` \
         would leave the runtime tests green while shipping the \
         unconditional abort of #682 comment 3669770201: a failed \
         writer spawn downgrading a working Rust hotkey install to the \
         Python fallback at log levels where no callback diagnostic would \
         have been written at all. Offending function body:\n{}",
        body.raw
    );
}
