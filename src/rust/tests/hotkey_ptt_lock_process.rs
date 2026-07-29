//! Cross-process tests for the push-to-talk ownership lock.
//!
//! The unit tests in `hotkey/ptt_lock/mod_tests.rs` cover everything a
//! single process can demonstrate — refusal, the named holder, release on
//! drop, release on unwind — hermetically, because both platforms scope
//! these locks to the file handle rather than the process.
//!
//! Two things they cannot prove, and this file does:
//!
//! 1. A **separate process** is refused. That is the actual 2026-07-29
//!    scenario (`whisper-dictate-gui.exe` plus `whisper-dictate.exe
//!    dictate-run`), and a same-process test is only a proxy for it.
//! 2. The lock survives neither `SIGKILL` nor `TerminateProcess`. Nothing
//!    in our code runs on that path — only the kernel closing the handle
//!    releases it — so it needs a process that is genuinely killed. This is
//!    the "a stale lock that blocks every future launch is worse than the
//!    bug" guarantee.
//!
//! ## Why the test binary re-executes itself
//!
//! The holder has to be a real OS process, and it has to take the lock
//! through the SAME production code path. Re-running this test binary with
//! `--ignored --exact` gives us both without adding a probe subcommand to
//! the shipping CLI surface, which would be a permanent public-API cost
//! paid for one test.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use whisper_dictate_app::hotkey::ptt_lock::{acquire_at, Acquisition, HolderRecord};

/// Env var carrying the temp directory from the parent test to the child
/// holder process. Its presence is also what tells the child which mode to
/// run in.
const HOLD_DIR_ENV: &str = "VOICEPI_TEST_PTT_HOLD_DIR";

/// Printed by the child once it owns the lock. The parent blocks on this
/// rather than sleeping, so the test is not a race on process startup time.
const HELD_MARKER: &str = "PTT_LOCK_HELD";

/// How long the parent waits for the child's marker before giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the parent waits for the lock to come free after killing the
/// child. Generous: on Windows handle teardown is asynchronous with respect
/// to `TerminateProcess` returning.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on the child's life, so a parent that dies mid-test cannot
/// strand a process holding a lock in a CI temp dir.
const CHILD_MAX_LIFETIME: Duration = Duration::from_secs(120);

fn lock_path(dir: &Path) -> PathBuf {
    dir.join("whisper-dictate-ptt-crossprocess.lock")
}

fn owner_path(dir: &Path) -> PathBuf {
    dir.join("whisper-dictate-ptt-crossprocess.owner")
}

/// The holder half. `#[ignore]` so a normal `cargo test` never runs it;
/// the parent invokes it explicitly with `--ignored --exact`.
///
/// Takes the lock through the production `acquire_at`, announces it, then
/// parks. It is expected to be killed rather than to return.
#[test]
#[ignore = "child half of ptt_lock_is_released_when_the_holder_is_killed; run by the parent test"]
fn ptt_lock_holder_child() {
    let Some(dir) = std::env::var_os(HOLD_DIR_ENV) else {
        // Somebody ran the whole suite with `--ignored`. Nothing to hold.
        return;
    };
    let dir = PathBuf::from(dir);
    let holder = HolderRecord::new(
        std::process::id(),
        "whisper-dictate-gui",
        "none",
        "win_registerhotkey",
        "f9",
    );
    let _lock = match acquire_at(&lock_path(&dir), &owner_path(&dir), holder) {
        Acquisition::Acquired(lock) => lock,
        other => panic!("child could not take the lock: {other:?}"),
    };
    // Stdout is piped; flush explicitly or the parent blocks on a buffer.
    println!("{HELD_MARKER} {}", std::process::id());
    use std::io::Write;
    let _ = std::io::stdout().flush();
    std::thread::sleep(CHILD_MAX_LIFETIME);
}

/// Spawn the child holder and block until it reports ownership. Returns
/// the child plus its PID.
fn spawn_holder(dir: &Path) -> (Child, u32) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut child = Command::new(exe)
        .args([
            "ptt_lock_holder_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(HOLD_DIR_ENV, dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the holder child");

    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            // Searched rather than prefix-matched: under `--nocapture`
            // libtest writes `test <name> ... ` with no trailing newline
            // before handing control to the test, so the marker lands
            // part-way through the line rather than at its start.
            if let Some(idx) = line.find(HELD_MARKER) {
                let rest = &line[idx + HELD_MARKER.len()..];
                let pid = rest.trim().parse::<u32>().unwrap_or(0);
                let _ = tx.send(pid);
                return;
            }
        }
    });

    match rx.recv_timeout(READY_TIMEOUT) {
        Ok(pid) => (child, pid),
        Err(err) => {
            let _ = child.kill();
            panic!("holder child never reported ownership within {READY_TIMEOUT:?}: {err}");
        }
    }
}

/// Try to take the lock in THIS process, returning the outcome.
fn try_acquire_here(dir: &Path) -> Acquisition {
    acquire_at(
        &lock_path(dir),
        &owner_path(dir),
        HolderRecord::new(
            std::process::id(),
            "whisper-dictate",
            "dictate-run",
            "rdev",
            "f9",
        ),
    )
}

#[test]
fn a_separate_process_is_refused_and_named_then_released_when_killed() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let (mut child, child_pid) = spawn_holder(dir.path());

    // 1. A DIFFERENT process holds it: we must be refused, and we must be
    //    able to name the process the user has to quit.
    match try_acquire_here(dir.path()) {
        Acquisition::Held {
            holder: Some(holder),
            ..
        } => {
            assert_eq!(
                holder.pid, child_pid,
                "the refusal must name the process that actually holds the lock"
            );
            assert_eq!(holder.exe, "whisper-dictate-gui");
            assert_eq!(holder.driver, "win_registerhotkey");
            assert!(holder.describe().contains(&format!("pid {child_pid}")));
        }
        other => panic!("a second process must be refused, got {other:?}"),
    }

    // 2. Kill it outright. No destructor runs, no cleanup code of ours
    //    executes -- only the kernel closing the handle can release the
    //    lock. If it could not, every future launch on this machine would
    //    be blocked, which is strictly worse than the bug being fixed.
    child.kill().expect("kill the holder");
    child.wait().expect("reap the holder");

    let deadline = Instant::now() + RELEASE_TIMEOUT;
    loop {
        match try_acquire_here(dir.path()) {
            Acquisition::Acquired(lock) => {
                // The advisory record left behind by the killed process
                // must not have blocked us -- the lock is the decision.
                assert_eq!(lock.holder().pid, std::process::id());
                break;
            }
            other if Instant::now() >= deadline => {
                panic!("lock was not released after the holder was killed: {other:?}");
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
