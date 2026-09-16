//! Tests for the sink-facing capture seam.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::{close, close_on_unwind, open, RecordingCapture, RecordingCaptureHandle};

struct Counting {
    opens: AtomicUsize,
    closes: AtomicUsize,
    open_result: bool,
}

impl RecordingCapture for Counting {
    fn open_for_recording(&self) -> bool {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.open_result
    }

    fn close_for_recording(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

fn handle(open_result: bool) -> (Arc<Counting>, RecordingCaptureHandle) {
    let counting = Arc::new(Counting {
        opens: AtomicUsize::new(0),
        closes: AtomicUsize::new(0),
        open_result,
    });
    let handle: RecordingCaptureHandle = counting.clone();
    (counting, handle)
}

#[test]
fn without_capture_open_succeeds_and_close_does_nothing() {
    assert!(open(None));
    close(None);
    drop(close_on_unwind(None));
}

#[test]
fn open_and_close_are_forwarded_to_the_capture() {
    let (counting, handle) = handle(true);
    assert!(open(Some(&handle)));
    close(Some(&handle));
    assert_eq!(counting.opens.load(Ordering::SeqCst), 1);
    assert_eq!(counting.closes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failed_open_is_reported_to_the_caller() {
    let (counting, handle) = handle(false);
    assert!(!open(Some(&handle)));
    assert_eq!(counting.opens.load(Ordering::SeqCst), 1);
}

#[test]
fn a_panicking_action_closes_the_microphone_while_unwinding() {
    let (counting, handle) = handle(true);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = close_on_unwind(Some(&handle));
        panic!("live settings reload failed");
    }));
    assert!(result.is_err());
    assert_eq!(counting.closes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_normal_return_leaves_closing_to_the_action() {
    let (counting, handle) = handle(true);
    {
        let _guard = close_on_unwind(Some(&handle));
    }
    assert_eq!(counting.closes.load(Ordering::SeqCst), 0);
}
