//! Companion tests for `hotkey/manager/driver_common.rs`: the bounded
//! command/ack round-trip every register / unregister call goes through.

use super::*;
use std::time::Instant;

/// Upper bound the test waits for `send_and_ack` itself to return. Far above
/// the injected ack timeout, so only an unbounded wait trips it.
const CALL_DEADLINE: Duration = Duration::from_secs(5);

/// Run `send_and_ack` for an `Unregister` on a helper thread and return its
/// result plus elapsed time, or `None` if it did not return within
/// [`CALL_DEADLINE`].
fn unregister_with_timeout(
    handle: &ManagerHandle,
    timeout: Duration,
) -> Option<(Result<(), AckError>, Duration)> {
    let handle = handle.clone();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let started = Instant::now();
        let result = handle.send_and_ack(|ack| ManagerCommand::Unregister { ack }, timeout);
        let _ = done_tx.send((result, started.elapsed()));
    });
    done_rx.recv_timeout(CALL_DEADLINE).ok()
}

#[test]
fn send_and_ack_times_out_when_the_driver_thread_never_acks() {
    // A wedged driver thread: it keeps the command receiver alive (so the
    // send succeeds and the ack sender stays parked in the queue) but never
    // services it.
    let (handle, _stalled_rx) = manager_channel();
    let timeout = Duration::from_millis(50);

    let (result, elapsed) = unregister_with_timeout(&handle, timeout)
        .expect("send_and_ack must return instead of blocking on a stalled driver thread");

    assert_eq!(result, Err(AckError::Timeout(timeout)));
    assert!(
        elapsed >= timeout && elapsed < Duration::from_secs(2),
        "timeout must fire after roughly {timeout:?}, took {elapsed:?}"
    );
}

#[test]
fn send_and_ack_returns_ok_when_the_driver_thread_acks() {
    let (handle, cmd_rx) = manager_channel();
    let driver = thread::spawn(move || match cmd_rx.recv() {
        Ok(ManagerCommand::Unregister { ack }) => {
            let _ = ack.send(Ok(()));
        }
        Ok(_) => panic!("expected an Unregister command"),
        Err(e) => panic!("command channel closed: {e}"),
    });

    let (result, _) = unregister_with_timeout(&handle, CALL_DEADLINE)
        .expect("send_and_ack must return once the driver acks");

    assert_eq!(result, Ok(()));
    driver.join().expect("fake driver thread");
}

#[test]
fn send_and_ack_surfaces_a_driver_rejection_unchanged() {
    let (handle, cmd_rx) = manager_channel();
    let driver = thread::spawn(move || {
        if let Ok(ManagerCommand::Unregister { ack }) = cmd_rx.recv() {
            let _ = ack.send(Err("rejected by driver".to_owned()));
        }
    });

    let (result, _) = unregister_with_timeout(&handle, CALL_DEADLINE)
        .expect("send_and_ack must return once the driver acks");

    assert_eq!(
        result,
        Err(AckError::Rejected("rejected by driver".to_owned()))
    );
    driver.join().expect("fake driver thread");
}
