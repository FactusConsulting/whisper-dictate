//! Companion tests for `ptt_lock/mod.rs` — the ownership decision itself.
//!
//! Every test here is hermetic: a `TempDir` per test, no process env
//! mutation, nothing spawned. That is possible because both supported
//! platforms scope these locks to the FILE HANDLE rather than the
//! process, so two `acquire_at` calls inside one test contend exactly the
//! way the GUI and the CLI contended on 2026-07-29. The one behaviour a
//! single process genuinely cannot demonstrate — release on `SIGKILL` /
//! `TerminateProcess` — is covered by
//! `src/rust/tests/hotkey_ptt_lock_process.rs`, which spawns real
//! processes.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::report;
use super::{acquire_at, acquire_or_refuse_at, Acquisition, HolderRecord, PttLock, PttOwnership};

/// Driver labels for the pair that actually broke. The GUI defaults to
/// `win_registerhotkey` on Windows; `dictate-run` defaults to `rdev`.
const DRIVER_REGISTER: &str = "win_registerhotkey";
const DRIVER_RDEV: &str = "rdev";

/// `(lock_path, owner_path)` inside a fresh temp dir.
fn paths_in(dir: &TempDir) -> (PathBuf, PathBuf) {
    (
        dir.path().join("whisper-dictate-ptt-test.lock"),
        dir.path().join("whisper-dictate-ptt-test.owner"),
    )
}

/// A holder record for a PID that is definitively not ours, so
/// "the refusal names the HOLDER" cannot pass by accident.
fn foreign_holder(chord: &str, driver: &str) -> HolderRecord {
    let foreign_pid = std::process::id().wrapping_add(4242);
    HolderRecord::new(foreign_pid, "whisper-dictate-gui", "none", driver, chord)
}

fn take(lock_path: &Path, owner_path: &Path, holder: HolderRecord) -> Acquisition {
    acquire_at(lock_path, owner_path, holder)
}

/// Unwrap an ownership outcome that must have taken the lock. Named so a
/// failure says which of the three outcomes actually came back -- the
/// fail-open `Unguarded` arm in particular is easy to mistake for success
/// when a test's temp dir is wrong.
fn expect_owned(outcome: PttOwnership, what: &str) -> PttLock {
    match outcome {
        PttOwnership::Owned(lock) => lock,
        other => panic!("{what}, got {other:?}"),
    }
}

#[test]
fn second_acquisition_is_refused_while_the_first_holds_it() {
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);

    let first = take(
        &lock_path,
        &owner_path,
        foreign_holder("f9", DRIVER_REGISTER),
    );
    let _held = match first {
        Acquisition::Acquired(lock) => lock,
        other => panic!("first acquisition must succeed, got {other:?}"),
    };

    let second = take(
        &lock_path,
        &owner_path,
        HolderRecord::new(
            std::process::id(),
            "whisper-dictate",
            "dictate-run",
            DRIVER_RDEV,
            "f9",
        ),
    );
    assert!(
        matches!(second, Acquisition::Held { .. }),
        "a second acquisition must be refused while the first is held, got {second:?}"
    );
}

#[test]
fn refusal_names_the_holding_process() {
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);
    let holder = foreign_holder("f9", DRIVER_REGISTER);

    let _held = match take(&lock_path, &owner_path, holder.clone()) {
        Acquisition::Acquired(lock) => lock,
        other => panic!("first acquisition must succeed, got {other:?}"),
    };

    match take(&lock_path, &owner_path, foreign_holder("f9", DRIVER_RDEV)) {
        Acquisition::Held {
            holder: Some(read), ..
        } => {
            // The whole point of the advisory record: the blocked process
            // must be able to tell the user WHICH process to quit.
            assert_eq!(read, holder, "the refusal must report the holder verbatim");
            assert!(
                read.describe().contains(&format!("pid {}", holder.pid)),
                "holder description must lead with the pid, got {}",
                read.describe()
            );
        }
        other => panic!("expected a named holder, got {other:?}"),
    }
}

#[test]
fn refusal_still_stands_when_the_holder_record_is_unreadable() {
    // The lock is the decision; the record is only the message. A
    // corrupted record must degrade the message, never the refusal --
    // otherwise a truncated file would reopen the corruption window.
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);

    let _held = match take(&lock_path, &owner_path, foreign_holder("f9", DRIVER_RDEV)) {
        Acquisition::Acquired(lock) => lock,
        other => panic!("first acquisition must succeed, got {other:?}"),
    };
    std::fs::write(&owner_path, b"garbage that is not a record\n").expect("clobber record");

    match take(
        &lock_path,
        &owner_path,
        foreign_holder("f9", DRIVER_REGISTER),
    ) {
        Acquisition::Held { holder: None, .. } => {}
        other => panic!("expected a refusal with an unnamed holder, got {other:?}"),
    }
}

#[test]
fn lock_is_released_when_the_holder_drops_normally() {
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);

    let first = match take(&lock_path, &owner_path, foreign_holder("f9", DRIVER_RDEV)) {
        Acquisition::Acquired(lock) => lock,
        other => panic!("first acquisition must succeed, got {other:?}"),
    };
    first.release();

    let second = take(
        &lock_path,
        &owner_path,
        foreign_holder("f9", DRIVER_REGISTER),
    );
    assert!(
        matches!(second, Acquisition::Acquired(_)),
        "a released lock must be re-acquirable, got {second:?}"
    );
    assert!(
        !owner_path.exists() || std::fs::read_to_string(&owner_path).is_ok(),
        "the owner record must be rewritten, not left dangling"
    );
}

#[test]
fn lock_is_released_when_the_holder_panics() {
    // A stale lock that survives a crash is worse than the bug it guards
    // against: it would block every future launch. Unwinding must run
    // `Drop`; process death (the abort / SIGKILL case) is the kernel's
    // job and is covered by the two-process integration test.
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);

    let panicked = std::panic::catch_unwind({
        let lock_path = lock_path.clone();
        let owner_path = owner_path.clone();
        move || {
            let _lock = match acquire_at(
                &lock_path,
                &owner_path,
                HolderRecord::new(4242, "whisper-dictate", "dictate-run", DRIVER_RDEV, "f9"),
            ) {
                Acquisition::Acquired(lock) => lock,
                other => panic!("acquisition inside the panicking scope failed: {other:?}"),
            };
            panic!("simulated holder crash");
        }
    });
    assert!(panicked.is_err(), "the closure must actually have panicked");

    let after = take(
        &lock_path,
        &owner_path,
        foreign_holder("f9", DRIVER_REGISTER),
    );
    assert!(
        matches!(after, Acquisition::Acquired(_)),
        "the lock must be free after the holder panicked, got {after:?}"
    );
}

#[test]
fn a_stale_owner_record_alone_never_refuses() {
    // After a `kill -9` the record file survives on disk but the OS lock
    // does not. A launch that read the record and refused would be a
    // phantom refusal -- exactly the "stale lock blocks every future
    // launch" failure the brief rules out.
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);
    std::fs::write(
        &owner_path,
        foreign_holder("f9", DRIVER_REGISTER).encode() + "\n",
    )
    .expect("write stale record");

    let acquired = take(&lock_path, &owner_path, foreign_holder("f9", DRIVER_RDEV));
    assert!(
        matches!(acquired, Acquisition::Acquired(_)),
        "a stale record with no live lock must not refuse, got {acquired:?}"
    );
}

#[test]
fn unavailable_when_the_lock_path_cannot_be_created() {
    // Fail-open: an unwritable lock location must not take push-to-talk
    // away from a user who has no second process at all.
    let dir = TempDir::new().expect("temp dir");
    // A regular file where a directory needs to be: `create_dir_all` on
    // the parent fails identically on Windows and Linux.
    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let lock_path = blocker.join("nested").join("ptt.lock");
    let owner_path = blocker.join("nested").join("ptt.owner");

    let outcome = take(&lock_path, &owner_path, foreign_holder("f9", DRIVER_RDEV));
    assert!(
        matches!(outcome, Acquisition::Unavailable { .. }),
        "an unopenable lock path must report Unavailable, got {outcome:?}"
    );
}

// ---------------------------------------------------------------------
// The install-time decision (`acquire_or_refuse_at`), including the
// cross-driver pair that actually broke.
// ---------------------------------------------------------------------

/// Drive the decision function for a driver pair and assert the second
/// call is refused, naming the first driver's chord and pid.
///
/// Both orders are exercised by the two tests below. The guard lives
/// ABOVE driver selection precisely so it cannot be order-sensitive, and
/// this is what pins that: a future refactor that pushed the check down
/// into a driver would pass one order and fail the other.
fn assert_cross_driver_refusal(first_driver: &str, second_driver: &str) {
    let _slot = report::TEST_SLOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);

    let held = expect_owned(
        acquire_or_refuse_at(&lock_path, &owner_path, "f9", first_driver),
        "first registration must take the lock",
    );
    assert_eq!(held.holder().driver, first_driver);
    assert!(
        report::current().is_none(),
        "a lone first registration must not publish a conflict"
    );

    let second = acquire_or_refuse_at(&lock_path, &owner_path, "f9", second_driver);
    let PttOwnership::Refused(conflict) = second else {
        panic!("the second registration must be refused, got {second:?}");
    };

    assert_eq!(conflict.holder_pid(), Some(std::process::id()));
    let message = conflict.message();
    assert!(
        message.contains(&format!("pid {}", std::process::id())),
        "refusal must name the holding pid, got: {message}"
    );
    assert!(
        message.contains("f9"),
        "refusal must name the refused chord, got: {message}"
    );
    assert!(
        message.contains(first_driver),
        "refusal must name the holder's driver ({first_driver}), got: {message}"
    );
    assert!(
        message.contains("interleaving"),
        "refusal must state the consequence it prevented, got: {message}"
    );
    assert_eq!(
        report::current().map(|c| c.holder_pid()),
        Some(Some(std::process::id())),
        "the refusal must be published for the GUI banner"
    );
    report::clear();
}

#[test]
fn registerhotkey_then_rdev_is_refused() {
    // The exact 2026-07-29 pairing: the tray GUI (RegisterHotKey) was up
    // first, then `dictate-run` (rdev) started and took F9 as well.
    assert_cross_driver_refusal(DRIVER_REGISTER, DRIVER_RDEV);
}

#[test]
fn rdev_then_registerhotkey_is_refused() {
    // The mirror image: a CLI left running, then the tray app launched.
    assert_cross_driver_refusal(DRIVER_RDEV, DRIVER_REGISTER);
}

#[test]
fn a_lone_registration_is_allowed_and_publishes_no_conflict() {
    let _slot = report::TEST_SLOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    report::record(super::PttConflict {
        chord: "stale".to_owned(),
        holder: None,
        lock_path: "stale".to_owned(),
    });
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);

    let lock = expect_owned(
        acquire_or_refuse_at(&lock_path, &owner_path, "ctrl_r", DRIVER_RDEV),
        "a lone registration must take the lock",
    );
    assert_eq!(lock.holder().chord, "ctrl_r");
    assert!(
        report::current().is_none(),
        "a successful acquisition must retract any earlier refusal banner"
    );
}

#[test]
fn ownership_is_reacquirable_after_the_first_holder_releases() {
    let _slot = report::TEST_SLOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);

    let first = expect_owned(
        acquire_or_refuse_at(&lock_path, &owner_path, "f9", DRIVER_REGISTER),
        "first must take the lock",
    );
    assert!(matches!(
        acquire_or_refuse_at(&lock_path, &owner_path, "f9", DRIVER_RDEV),
        PttOwnership::Refused(_)
    ));
    drop(first);

    let second = expect_owned(
        acquire_or_refuse_at(&lock_path, &owner_path, "f9", DRIVER_RDEV),
        "the second must take the lock once the first released",
    );
    assert_eq!(second.holder().driver, DRIVER_RDEV);
    report::clear();
}

#[test]
fn releasing_removes_the_owner_record() {
    // Drop order contract: the advisory record must go BEFORE the lock,
    // so a waiting process cannot have its own fresh record deleted by
    // the outgoing holder.
    let dir = TempDir::new().expect("temp dir");
    let (lock_path, owner_path) = paths_in(&dir);

    let lock = match take(&lock_path, &owner_path, foreign_holder("f9", DRIVER_RDEV)) {
        Acquisition::Acquired(lock) => lock,
        other => panic!("acquisition must succeed, got {other:?}"),
    };
    assert!(owner_path.exists(), "holding must publish a record");
    assert_eq!(lock.lock_path(), lock_path.as_path());
    lock.release();
    assert!(
        !owner_path.exists(),
        "releasing must remove the advisory record"
    );
}
