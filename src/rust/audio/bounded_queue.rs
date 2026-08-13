//! Small bounded channel that keeps the newest items without ever blocking a
//! producer.
//!
//! Audio callbacks cannot wait for a consumer. `crossbeam_channel::bounded`
//! supplies the fixed allocation and non-blocking primitives; the producer
//! keeps a private receiver clone so a full queue can evict its oldest item
//! before retrying the new one.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{
    Receiver, RecvError, RecvTimeoutError, Sender, TryRecvError, TrySendError,
};

/// Monotonic count of items evicted because a bounded queue was full.
#[derive(Clone, Debug, Default)]
pub struct OverflowMetric(Arc<AtomicU64>);

impl OverflowMetric {
    pub fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    fn increment(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Non-blocking producer for a fixed-capacity, drop-oldest queue.
pub struct LatestSender<T> {
    tx: Sender<T>,
    eviction_rx: Receiver<T>,
    consumer_alive: Arc<AtomicBool>,
    overflow: OverflowMetric,
}

impl<T> Clone for LatestSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            eviction_rx: self.eviction_rx.clone(),
            consumer_alive: Arc::clone(&self.consumer_alive),
            overflow: self.overflow.clone(),
        }
    }
}

impl<T> LatestSender<T> {
    /// Enqueue `item` immediately. If the queue is full, evict the oldest item
    /// and retain the new one. Returns `Ok(true)` when at least one item was
    /// evicted, or the item itself when the consumer has gone away.
    pub fn try_send_latest(&self, mut item: T) -> Result<bool, T> {
        if !self.consumer_alive.load(Ordering::Acquire) {
            return Err(item);
        }

        let mut overflowed = false;
        loop {
            match self.tx.try_send(item) {
                Ok(()) => return Ok(overflowed),
                Err(TrySendError::Disconnected(returned)) => return Err(returned),
                Err(TrySendError::Full(returned)) => {
                    item = returned;
                    match self.eviction_rx.try_recv() {
                        Ok(_) => {
                            self.overflow.increment();
                            overflowed = true;
                        }
                        // The real consumer won the race for the oldest item.
                        // Retry the non-blocking send; no mutex or wait occurs.
                        Err(TryRecvError::Empty) => {}
                        Err(TryRecvError::Disconnected) => return Err(item),
                    }
                }
            }
        }
    }

    pub fn overflow_metric(&self) -> OverflowMetric {
        self.overflow.clone()
    }
}

/// Sole consuming endpoint for a [`LatestSender`].
pub struct LatestReceiver<T> {
    rx: Receiver<T>,
    consumer_alive: Arc<AtomicBool>,
    overflow: OverflowMetric,
}

impl<T> LatestReceiver<T> {
    pub fn recv(&self) -> Result<T, RecvError> {
        self.rx.recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.rx.try_recv()
    }

    pub fn overflow_metric(&self) -> OverflowMetric {
        self.overflow.clone()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rx.len()
    }
}

impl<T> Drop for LatestReceiver<T> {
    fn drop(&mut self) {
        self.consumer_alive.store(false, Ordering::Release);
    }
}

/// Create a fixed-capacity channel whose producer always retains the newest
/// items and never waits for space.
pub fn bounded_latest<T>(capacity: usize) -> (LatestSender<T>, LatestReceiver<T>) {
    assert!(
        capacity > 0,
        "bounded audio queue capacity must be positive"
    );
    let (tx, rx) = crossbeam_channel::bounded(capacity);
    let consumer_alive = Arc::new(AtomicBool::new(true));
    let overflow = OverflowMetric::default();
    (
        LatestSender {
            tx,
            eviction_rx: rx.clone(),
            consumer_alive: Arc::clone(&consumer_alive),
            overflow: overflow.clone(),
        },
        LatestReceiver {
            rx,
            consumer_alive,
            overflow,
        },
    )
}

#[cfg(test)]
#[path = "bounded_queue_tests.rs"]
mod tests;
