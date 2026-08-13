use std::time::{Duration, Instant};

use super::bounded_latest;

#[test]
fn full_queue_drops_oldest_and_counts_overflow() {
    let (tx, rx) = bounded_latest(2);
    assert_eq!(tx.try_send_latest(1), Ok(false));
    assert_eq!(tx.try_send_latest(2), Ok(false));
    assert_eq!(tx.try_send_latest(3), Ok(true));

    assert_eq!(rx.len(), 2);
    assert_eq!(rx.recv(), Ok(2));
    assert_eq!(rx.recv(), Ok(3));
    assert_eq!(rx.overflow_metric().count(), 1);
}

#[test]
fn saturated_producer_remains_non_blocking() {
    let (tx, rx) = bounded_latest(4);
    for value in 0..4 {
        tx.try_send_latest(value).expect("consumer alive");
    }

    let started = Instant::now();
    for value in 4..10_004 {
        tx.try_send_latest(value).expect("consumer alive");
    }

    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a full queue must never wait for its stalled consumer"
    );
    assert_eq!(rx.len(), 4);
    assert_eq!(rx.overflow_metric().count(), 10_000);
    assert_eq!(rx.recv(), Ok(10_000));
    assert_eq!(rx.recv(), Ok(10_001));
    assert_eq!(rx.recv(), Ok(10_002));
    assert_eq!(rx.recv(), Ok(10_003));
}

#[test]
fn producer_observes_dropped_consumer_without_waiting() {
    let (tx, rx) = bounded_latest(1);
    drop(rx);

    assert_eq!(tx.try_send_latest(7), Err(7));
}
