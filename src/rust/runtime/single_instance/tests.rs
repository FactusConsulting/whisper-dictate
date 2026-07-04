//! Behavioural + integration tests for the single-instance gate.
//!
//! These sit alongside the finer-grained framing / lockfile unit tests
//! in the child modules; the tests here drive the full [`try_acquire`]
//! + accept-loop path with a per-test tempdir so they run safely in
//!   parallel and never touch a real `$XDG_RUNTIME_DIR`.

use super::*;
use std::sync::MutexGuard;
use std::time::Duration;
use tempfile::TempDir;

/// Test guard that (1) serialises access to the crate-wide env lock and
/// (2) points the single-instance runtime dir at a per-test tempdir.
/// The Drop impl clears the env var so leaks between tests can't
/// happen even on panic.
struct TestEnv {
    _guard: MutexGuard<'static, ()>,
    _dir: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .expect("ENV_LOCK poisoned");
        let dir = TempDir::new().expect("tempdir");
        std::env::set_var(RUNTIME_DIR_OVERRIDE_ENV, dir.path());
        Self {
            _guard: guard,
            _dir: dir,
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        std::env::remove_var(RUNTIME_DIR_OVERRIDE_ENV);
    }
}

fn assert_acquired(outcome: AcquireOutcome) -> SingleInstance {
    match outcome {
        AcquireOutcome::Acquired(guard) => guard,
        AcquireOutcome::Forwarded => panic!("expected Acquired, got Forwarded"),
    }
}

fn assert_forwarded(outcome: AcquireOutcome) {
    match outcome {
        AcquireOutcome::Forwarded => {}
        AcquireOutcome::Acquired(_) => panic!("expected Forwarded, got Acquired"),
    }
}

#[test]
fn first_call_acquires_and_writes_lockfile() {
    let _env = TestEnv::new();
    let guard = assert_acquired(try_acquire(vec![]).unwrap());

    // Lockfile now exists and records our port.
    let runtime_dir = lockfile::resolve_runtime_dir().unwrap();
    let lock_path = lockfile::lockfile_path(&runtime_dir);
    let data = lockfile::read_lockfile(&lock_path).unwrap().unwrap();
    assert_eq!(data.port, guard.port());
    assert_eq!(data.pid, std::process::id());
    assert!(!data.token.is_empty());
}

#[test]
fn dropping_guard_removes_lockfile() {
    let _env = TestEnv::new();
    let runtime_dir = lockfile::resolve_runtime_dir().unwrap();
    let lock_path = lockfile::lockfile_path(&runtime_dir);

    {
        let _guard = assert_acquired(try_acquire(vec![]).unwrap());
        assert!(lock_path.exists());
    }
    // Drop → accept thread joined → lockfile removed.
    assert!(!lock_path.exists());
}

#[test]
fn second_call_forwards_argv_to_first() {
    let _env = TestEnv::new();
    let first = assert_acquired(try_acquire(vec![]).unwrap());

    let forwarded_argv = vec!["--toggle-recording".to_owned()];
    assert_forwarded(try_acquire(forwarded_argv.clone()).unwrap());

    // The owning instance sees the forwarded argv on its receiver.
    let received = first
        .recv_timeout(Duration::from_secs(2))
        .expect("expected forwarded command");
    assert_eq!(received.argv, forwarded_argv);
}

#[test]
fn multiple_forwards_are_all_delivered_in_order() {
    let _env = TestEnv::new();
    let first = assert_acquired(try_acquire(vec![]).unwrap());

    for i in 0..5 {
        assert_forwarded(try_acquire(vec![format!("--nth={i}")]).unwrap());
    }

    let mut received = Vec::new();
    for _ in 0..5 {
        received.push(
            first
                .recv_timeout(Duration::from_secs(2))
                .expect("expected forwarded command")
                .argv,
        );
    }
    for (i, argv) in received.iter().enumerate() {
        assert_eq!(argv, &vec![format!("--nth={i}")]);
    }
}

#[test]
fn stale_lockfile_is_taken_over() {
    let _env = TestEnv::new();
    let runtime_dir = lockfile::resolve_runtime_dir().unwrap();
    let lock_path = lockfile::lockfile_path(&runtime_dir);

    // Plant a lockfile pointing at a port with nothing listening on it.
    // Grab a free port then drop the listener so the port is unbound.
    let dead_port = {
        let (listener, port) = socket::bind_loopback().unwrap();
        drop(listener);
        port
    };
    lockfile::write_lockfile(
        &lock_path,
        &LockData {
            pid: 999_999, // Highly unlikely to be a real running PID.
            port: dead_port,
            token: "stale".to_owned(),
        },
    )
    .unwrap();

    // We should ignore the stale lockfile and become the owner.
    let guard = assert_acquired(try_acquire(vec![]).unwrap());
    let fresh = lockfile::read_lockfile(&lock_path).unwrap().unwrap();
    assert_ne!(fresh.port, dead_port);
    assert_eq!(fresh.port, guard.port());
}

#[test]
fn wrong_token_is_rejected_and_no_command_delivered() {
    let _env = TestEnv::new();
    let first = assert_acquired(try_acquire(vec![]).unwrap());

    // Bypass the normal client path and send a request with a bad token
    // directly, so we prove the server drops it rather than delivering
    // to the owning instance.
    let err = socket::forward(first.port(), "bogus", &["--x".to_owned()]).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("token"),
        "expected token-related error, got: {err}"
    );

    // Nothing should have been queued.
    assert!(first.try_recv().is_none());
}

#[test]
fn owner_can_recv_forwarded_commands_via_try_recv_after_polling() {
    let _env = TestEnv::new();
    let first = assert_acquired(try_acquire(vec![]).unwrap());

    // Initially nothing pending.
    assert!(first.try_recv().is_none());

    assert_forwarded(try_acquire(vec!["--cancel".to_owned()]).unwrap());

    // Poll try_recv until the accept thread has surfaced the command.
    // The loop caps at ~2s so a genuine bug times out cleanly.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let received = loop {
        if let Some(cmd) = first.try_recv() {
            break cmd;
        }
        if std::time::Instant::now() > deadline {
            panic!("try_recv never surfaced the forwarded command");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(received.argv, vec!["--cancel".to_owned()]);
}

#[test]
fn drop_after_orphan_forward_does_not_panic() {
    // Regression: earlier drafts of the accept-loop held the sender
    // across the shutdown self-connect; if that self-connect races
    // with a genuine forward, the loop must still exit cleanly rather
    // than panicking.
    let _env = TestEnv::new();
    let first = assert_acquired(try_acquire(vec![]).unwrap());
    assert_forwarded(try_acquire(vec!["--drop-me".to_owned()]).unwrap());
    // Deliberately drop `first` without draining the queue.
    drop(first);
}
