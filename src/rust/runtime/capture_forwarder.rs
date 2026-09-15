//! Moves frames from an open capture stream into the dictation session.
//!
//! The forwarder never waits on the session mutex (transcription may hold it
//! for minutes). Because a stream is only open while recording (#323), a
//! frame that meets a briefly busy session is kept in a bounded pending
//! buffer and delivered once the lock is free, so the first spoken word and
//! the release tail are not lost.

#![cfg(feature = "audio-capture")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};

use super::capture_status::LOG_PREFIX;
use crate::audio::PipelineEvent;
use crate::dictate::session::{DictateSession, InjectBackend, TranscribeBackend};

/// Frames retained while the session is busy: 400 x 30 ms = 12 s.
pub(crate) const PENDING_FRAME_LIMIT: usize = 400;

/// Receiver of captured frames.
pub(crate) trait FrameSink: Send + Sync + 'static {
    /// Deliver one frame without blocking. `false` means the receiver is busy
    /// right now; the caller keeps the frame and retries later.
    fn try_push(&self, frame: &[f32]) -> bool;
}

/// Production sink: the shared dictation session.
pub(crate) struct SessionFrameSink<T: TranscribeBackend, I: InjectBackend> {
    session: Arc<Mutex<DictateSession<T, I>>>,
}

impl<T: TranscribeBackend, I: InjectBackend> SessionFrameSink<T, I> {
    pub(crate) fn new(session: Arc<Mutex<DictateSession<T, I>>>) -> Self {
        Self { session }
    }
}

impl<T, I> FrameSink for SessionFrameSink<T, I>
where
    T: TranscribeBackend + Send + 'static,
    I: InjectBackend + Send + 'static,
{
    fn try_push(&self, frame: &[f32]) -> bool {
        match self.session.try_lock() {
            Ok(mut guard) => {
                guard.push_frame(frame);
                true
            }
            Err(TryLockError::Poisoned(poison)) => {
                poison.into_inner().push_frame(frame);
                true
            }
            Err(TryLockError::WouldBlock) => false,
        }
    }
}

/// Why forwarding stopped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ForwardEnd {
    /// The stream was closed (channel disconnected).
    Closed,
    /// The capture device failed; per the pipeline contract no events follow.
    DeviceError(String),
}

/// Forward events until the stream closes or reports a device error.
pub(crate) fn forward_frames<F: FrameSink + ?Sized>(
    mut recv_next: impl FnMut() -> Option<PipelineEvent>,
    sink: &F,
) -> ForwardEnd {
    let mut pending = PendingFrames::default();
    while let Some(event) = recv_next() {
        match event {
            PipelineEvent::Frame(frame) => pending.deliver(sink, frame),
            PipelineEvent::DeviceError(message) => {
                pending.finish(sink);
                return ForwardEnd::DeviceError(message);
            }
        }
    }
    pending.finish(sink);
    ForwardEnd::Closed
}

#[derive(Default)]
struct PendingFrames {
    frames: VecDeque<Vec<f32>>,
    evicted: usize,
}

impl PendingFrames {
    fn deliver<F: FrameSink + ?Sized>(&mut self, sink: &F, frame: Vec<f32>) {
        self.flush(sink);
        if self.frames.is_empty() && sink.try_push(&frame) {
            return;
        }
        if self.frames.len() >= PENDING_FRAME_LIMIT {
            self.frames.pop_front();
            self.evicted += 1;
        }
        self.frames.push_back(frame);
    }

    fn flush<F: FrameSink + ?Sized>(&mut self, sink: &F) {
        while let Some(front) = self.frames.front() {
            if !sink.try_push(front) {
                break;
            }
            self.frames.pop_front();
        }
    }

    fn finish<F: FrameSink + ?Sized>(&mut self, sink: &F) {
        self.flush(sink);
        let discarded = self.frames.len() + self.evicted;
        if discarded > 0 {
            crate::diag::log!(
                "{LOG_PREFIX} discarded {discarded} captured frame(s) because the session stayed busy"
            );
        }
        self.frames.clear();
        self.evicted = 0;
    }
}

#[cfg(test)]
#[path = "capture_forwarder_tests.rs"]
mod tests;
