//! Moves frames from an open capture stream into the dictation session.
//!
//! The forwarder never waits on the session mutex (transcription may hold it
//! for minutes). Because a stream is only open while recording (#323), a
//! frame that meets a briefly busy session is kept in a bounded pending
//! buffer and delivered once the lock is free, so the first spoken word and
//! the release tail are not lost. Once the recording reaches `max_record_s`
//! forwarding ends so the lifecycle can close the microphone.

#![cfg(feature = "audio-capture")]
// The production caller (`rust_session_audio`) needs the full backend set.
#![cfg_attr(
    not(all(feature = "whisper-rs-local", feature = "rust-injection")),
    allow(dead_code)
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};

use super::capture_status::LOG_PREFIX;
use crate::audio::PipelineEvent;
use crate::dictate::session::{DictateSession, InjectBackend, TranscribeBackend};

/// Frames retained while the session is busy: 400 x 30 ms = 12 s.
pub(crate) const PENDING_FRAME_LIMIT: usize = 400;

/// What happened to one delivered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushOutcome {
    Accepted,
    /// The receiver is busy right now; keep the frame and retry.
    Busy,
    /// Accepted, and the recording has reached its size cap.
    RecordingFull,
}

/// Receiver of captured frames.
pub(crate) trait FrameSink: Send + Sync + 'static {
    /// Deliver one frame without blocking.
    fn try_push(&self, frame: &[f32]) -> PushOutcome;
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
    fn try_push(&self, frame: &[f32]) -> PushOutcome {
        let mut guard = match self.session.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(TryLockError::WouldBlock) => return PushOutcome::Busy,
        };
        guard.push_frame(frame);
        if guard.recording_buffer_full() {
            PushOutcome::RecordingFull
        } else {
            PushOutcome::Accepted
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
    /// The recording reached `max_record_s`; later audio would be discarded.
    RecordingFull,
}

/// Forward events until the stream closes, fails, or the recording is full.
#[cfg(test)]
pub(crate) fn forward_frames<F: FrameSink + ?Sized>(
    recv_next: impl FnMut() -> Option<PipelineEvent>,
    sink: &F,
) -> ForwardEnd {
    forward_frames_with_report(recv_next, sink).end
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ForwardReport {
    pub(crate) end: ForwardEnd,
    pub(crate) pending_frames_dropped: u64,
}

pub(crate) fn forward_frames_with_report<F: FrameSink + ?Sized>(
    mut recv_next: impl FnMut() -> Option<PipelineEvent>,
    sink: &F,
) -> ForwardReport {
    let mut pending = PendingFrames::default();
    while let Some(event) = recv_next() {
        match event {
            PipelineEvent::Frame(frame) => {
                if pending.deliver(sink, frame) {
                    let pending_frames_dropped = pending.evicted as u64;
                    pending.discard();
                    return ForwardReport {
                        end: ForwardEnd::RecordingFull,
                        pending_frames_dropped,
                    };
                }
            }
            PipelineEvent::DeviceError(message) => {
                let pending_frames_dropped = pending.finish(sink);
                return ForwardReport {
                    end: ForwardEnd::DeviceError(message),
                    pending_frames_dropped,
                };
            }
        }
    }
    let pending_frames_dropped = pending.finish(sink);
    ForwardReport {
        end: ForwardEnd::Closed,
        pending_frames_dropped,
    }
}

#[derive(Default)]
struct PendingFrames {
    frames: VecDeque<Vec<f32>>,
    evicted: usize,
}

impl PendingFrames {
    /// Returns true once the recording is full.
    fn deliver<F: FrameSink + ?Sized>(&mut self, sink: &F, frame: Vec<f32>) -> bool {
        if self.flush(sink) {
            return true;
        }
        if self.frames.is_empty() {
            match sink.try_push(&frame) {
                PushOutcome::Accepted => return false,
                PushOutcome::RecordingFull => return true,
                PushOutcome::Busy => {}
            }
        }
        if self.frames.len() >= PENDING_FRAME_LIMIT {
            self.frames.pop_front();
            self.evicted += 1;
        }
        self.frames.push_back(frame);
        false
    }

    /// Deliver queued frames in order; true once the recording is full.
    fn flush<F: FrameSink + ?Sized>(&mut self, sink: &F) -> bool {
        while let Some(front) = self.frames.front() {
            match sink.try_push(front) {
                PushOutcome::Accepted => {
                    self.frames.pop_front();
                }
                PushOutcome::RecordingFull => {
                    self.frames.pop_front();
                    return true;
                }
                PushOutcome::Busy => return false,
            }
        }
        false
    }

    fn finish<F: FrameSink + ?Sized>(&mut self, sink: &F) -> u64 {
        let recording_full = self.flush(sink);
        // The recording cap is intentional, but earlier overflow is still loss.
        let discarded = self.evicted + if recording_full { 0 } else { self.frames.len() };
        if discarded > 0 {
            crate::diag::log!(
                "{LOG_PREFIX} discarded {discarded} captured frame(s) because the session stayed busy"
            );
        }
        self.discard();
        discarded as u64
    }

    fn discard(&mut self) {
        self.frames.clear();
        self.evicted = 0;
    }
}

#[cfg(test)]
#[path = "capture_forwarder_tests.rs"]
mod tests;
