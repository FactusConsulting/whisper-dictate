//! Companion tests for `hotkey/handle.rs` — the push-to-talk ownership
//! wiring of the handle itself.
//!
//! The guard's own behaviour is covered exhaustively in
//! `ptt_lock/mod_tests.rs`. What CANNOT be covered there is that the
//! lock lives in `HotkeyHandle` and that teardown releases it in the
//! right order — a real install needs an OS listener that headless CI
//! does not have, so there is no behavioural seam to drive. The same
//! structural-scanner technique as `mod_tests.rs` pins those invariants.

use crate::diag_tests::scan_fn_body;

#[test]
fn the_handle_owns_the_lock_so_teardown_releases_push_to_talk() {
    // The lock has to live in `HotkeyHandle`, not in a local: that is what
    // ties its lifetime to the installed hotkey, so `Drop` (or process
    // death) hands push-to-talk back without an explicit release call
    // anywhere.
    let src = std::fs::read_to_string("src/rust/hotkey/handle.rs")
        .or_else(|_| std::fs::read_to_string("hotkey/handle.rs"))
        .expect("hotkey/handle.rs must be readable from the test working dir");
    let struct_start = src
        .find("pub struct HotkeyHandle {")
        .expect("HotkeyHandle must exist");
    let struct_end = src[struct_start..]
        .find(
            "
}",
        )
        .map(|i| struct_start + i)
        .expect("HotkeyHandle must be brace-terminated");
    assert!(
        src[struct_start..struct_end].contains("ptt_lock: PttOwnershipState"),
        "`HotkeyHandle` must own the push-to-talk lock; holding it in a          local would release ownership the moment the install returns,          leaving the chord free for a second process to take."
    );
}

#[test]
fn begin_shutdown_releases_ptt_before_long_thread_joins() {
    let body = scan_fn_body("src/rust/hotkey/handle.rs", "pub(crate) fn begin_shutdown");
    let unregister = body
        .code
        .find("self.manager.unregister()")
        .expect("shutdown must unregister the active binding");
    let release = body
        .code
        .find("self.ptt_lock.release()")
        .expect("shutdown must synchronously release PTT ownership");
    let coordinator_shutdown = body
        .code
        .find("self.coordinator.shutdown()")
        .expect("shutdown must close the coordinator input");
    assert!(
        unregister < release && release < coordinator_shutdown,
        "PTT ownership must be released immediately after unregister and before          asynchronous coordinator teardown"
    );
}
