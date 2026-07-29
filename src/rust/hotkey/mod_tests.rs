//! Companion tests for `hotkey/mod.rs` — specifically the wiring of the
//! push-to-talk ownership guard into the install funnel.
//!
//! The guard's own behaviour is covered exhaustively in
//! `ptt_lock/mod_tests.rs`. What CANNOT be covered there is that
//! `install_hotkey_with_raw_tap` actually calls it, and calls it in the
//! right place — a real install needs an OS listener (an X display, a
//! Windows message pump) that headless CI does not have, so there is no
//! behavioural seam to drive.
//!
//! So this is a structural scanner, the same technique
//! `inject_guard_tests` and `diag_tests` use for their own
//! must-happen-before invariants. It is deliberately narrow: it asserts
//! the call exists and precedes the two spawns, and nothing about how the
//! rest of the function is written.

use crate::diag_tests::scan_fn_body;

/// The install funnel every backend and every entry point passes through.
const INSTALL_FN: &str = "pub fn install_hotkey_with_raw_tap<F, R>(";

#[test]
fn the_install_funnel_takes_push_to_talk_ownership() {
    // If this call is ever removed, two whisper-dictate processes can hold
    // the same chord again and inject over each other character by
    // character with nothing in either log -- the 2026-07-29 report. The
    // unit tests around `ptt_lock` would all still pass, because they test
    // the lock rather than its use, so the regression has to be caught
    // here.
    let body = scan_fn_body("src/rust/hotkey/mod.rs", INSTALL_FN);
    assert!(
        body.code.contains("ptt_lock::acquire_or_refuse("),
        "`install_hotkey_with_raw_tap` must take push-to-talk ownership; \
         without it a second whisper-dictate process can register the same \
         chord and both will inject into the focused window at once."
    );
    assert!(
        body.code.contains("InstallError::AlreadyHeld"),
        "a refused acquisition must surface as `InstallError::AlreadyHeld` \
         so callers can tell it apart from the fallback-to-pynput errors."
    );
}

#[test]
fn ownership_is_taken_before_any_thread_is_spawned() {
    // Ordering is load-bearing twice over. A refused process must leave no
    // coordinator or listener thread behind, and the guard must sit ABOVE
    // driver selection -- the 2026-07-29 pair used two DIFFERENT drivers,
    // so a check pushed down into either one would be blind to the other.
    let body = scan_fn_body("src/rust/hotkey/mod.rs", INSTALL_FN);
    let acquire = body
        .code
        .find("ptt_lock::acquire_or_refuse(")
        .expect("the ownership acquisition must exist");
    let coordinator = body
        .code
        .find("spawn_coordinator(")
        .expect("the coordinator spawn must exist");
    let manager = body
        .code
        .find("spawn_manager_with_driver(")
        .expect("the manager spawn must exist");
    assert!(
        acquire < coordinator,
        "push-to-talk ownership must be taken BEFORE the coordinator thread \
         spawns, so a refused install leaves nothing running"
    );
    assert!(
        acquire < manager,
        "push-to-talk ownership must be taken BEFORE the OS listener spawns, \
         and above driver selection -- a guard inside a driver cannot see a \
         second process using the other driver"
    );
}

// ---------------------------------------------------------------------
// Suspend / resume ownership hand-back (Codex P2 #688).
//
// A stopped tray GUI has no registered chord, so it must not go on
// reserving one -- otherwise every later `dictate-run` is refused by a
// process that is not listening, which is a refusal that protects
// nothing. These drive the state machine directly with REAL locks in a
// temp dir; a full `suspend()` needs an OS listener that headless CI
// cannot provide.
// ---------------------------------------------------------------------

#[cfg(feature = "rust-hotkeys")]
mod ownership_handback {
    use crate::hotkey::ptt_lock::{acquire_at, Acquisition, HolderRecord};
    use crate::hotkey::PttOwnershipState;

    fn held_state(dir: &tempfile::TempDir) -> (PttOwnershipState, std::path::PathBuf) {
        let lock_path = dir.path().join("ptt.lock");
        let owner_path = dir.path().join("ptt.owner");
        let lock = match acquire_at(
            &lock_path,
            &owner_path,
            HolderRecord::new(4242, "whisper-dictate-gui", "none", "rdev", "f9"),
        ) {
            Acquisition::Acquired(lock) => lock,
            other => panic!("setup acquisition failed: {other:?}"),
        };
        (PttOwnershipState::Held(lock), lock_path)
    }

    fn can_acquire(lock_path: &std::path::Path) -> bool {
        let owner_path = lock_path.with_extension("owner");
        matches!(
            acquire_at(
                lock_path,
                &owner_path,
                HolderRecord::new(7, "whisper-dictate", "dictate-run", "rdev", "f9"),
            ),
            Acquisition::Acquired(_)
        )
    }

    #[test]
    fn suspending_a_held_handle_frees_the_chord_for_another_process() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let (mut state, lock_path) = held_state(&dir);
        assert!(
            !can_acquire(&lock_path),
            "a held state must block another process"
        );

        let released = state.release_for_suspend();
        assert!(released.is_some(), "suspending must yield the lock");
        assert!(
            state.needs_reacquire(),
            "a suspended handle must know it owes itself a re-acquire"
        );
        drop(released);

        assert!(
            can_acquire(&lock_path),
            "after suspend, a second whisper-dictate process must be able to take the chord"
        );
    }

    #[test]
    fn an_inactive_handle_never_starts_acquiring_on_resume() {
        // `Inactive` covers the fail-open install (unopenable lock) and
        // the test stub. Neither ever held ownership, so a resume that
        // reached for the real per-user lock would be acquiring something
        // it was never given -- and in the stub's case would contend with
        // the developer's own running tray app.
        let mut state = PttOwnershipState::Inactive;
        assert!(state.release_for_suspend().is_none());
        assert!(
            !state.needs_reacquire(),
            "an inactive handle must stay inactive across suspend/resume"
        );
    }

    #[test]
    fn a_held_handle_does_not_re_acquire_on_a_restart_resume() {
        // `resume` is also called on the restart path with no preceding
        // `suspend`. Re-acquiring there would mean dropping our own lock
        // first, opening a window for another process to take the chord
        // out from under a running session.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let (state, _lock_path) = held_state(&dir);
        assert!(!state.needs_reacquire());
        assert!(state.is_held());
    }
}

#[test]
fn suspend_releases_and_resume_retakes_push_to_talk_ownership() {
    // Structural counterpart to the state-machine tests above: they pin
    // the transitions, this pins that `suspend` / `resume` actually use
    // them. Neither method can be driven end to end without a live OS
    // listener.
    let suspend = scan_fn_body("src/rust/hotkey/mod.rs", "pub fn suspend(&self)");
    assert!(
        suspend.code.contains("release_for_suspend()"),
        "`suspend` must release push-to-talk ownership; a stopped GUI that \
         keeps the lock refuses every later dictate-run for no reason."
    );
    let resume = scan_fn_body("src/rust/hotkey/mod.rs", "pub fn resume(&self,");
    assert!(
        resume.code.contains("reacquire_ptt_ownership("),
        "`resume` must take ownership back, or a resumed GUI would listen \
         on a chord it no longer owns."
    );
    let acquire = resume
        .code
        .find("reacquire_ptt_ownership(")
        .expect("the re-acquire call must exist");
    let register = resume
        .code
        .find("self.manager.register(")
        .expect("the register call must exist");
    assert!(
        acquire < register,
        "ownership must be taken BEFORE the binding is registered, so a \
         refused resume never leaves a live listener behind"
    );
}

#[test]
fn the_phase_b_fallback_parks_python_on_an_ownership_refusal() {
    // The default Windows GUI path. Every other in-process install
    // failure hands the chord to pynput; on this one that would put a
    // second listener on it and re-create the bug. Codex P1 #688.
    let body = scan_fn_body("src/rust/runtime/supervisor.rs", "pub fn start(&mut self,");
    let refusal = body
        .code
        .find("InProcessInstallError::PttAlreadyHeld")
        .expect(
            "the Phase B fallback must special-case the ownership refusal; \
             without it a refused process registers the same chord through pynput",
        );
    let park = body.code[refusal..]
        .find("disable_python_hotkey(")
        .expect("the ownership-refusal branch must park the Python listener");
    assert!(
        park < 400,
        "the `disable_python_hotkey` call must belong to the ownership-refusal \
         branch, not to some later unrelated one"
    );
}

#[test]
fn the_handle_owns_the_lock_so_teardown_releases_push_to_talk() {
    // The lock has to live in `HotkeyHandle`, not in a local: that is what
    // ties its lifetime to the installed hotkey, so `Drop` (or process
    // death) hands push-to-talk back without an explicit release call
    // anywhere.
    let src = std::fs::read_to_string("src/rust/hotkey/mod.rs")
        .or_else(|_| std::fs::read_to_string("hotkey/mod.rs"))
        .expect("hotkey/mod.rs must be readable from the test working dir");
    let struct_start = src
        .find("pub struct HotkeyHandle {")
        .expect("HotkeyHandle must exist");
    let struct_end = src[struct_start..]
        .find("\n}")
        .map(|i| struct_start + i)
        .expect("HotkeyHandle must be brace-terminated");
    assert!(
        src[struct_start..struct_end].contains("ptt_lock: std::sync::Mutex<PttOwnershipState>"),
        "`HotkeyHandle` must own the push-to-talk lock; holding it in a \
         local would release ownership the moment the install returns, \
         leaving the chord free for a second process to take."
    );
}
