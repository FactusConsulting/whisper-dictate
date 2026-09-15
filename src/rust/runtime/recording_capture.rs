//! Seam between the coordinator action sink and the microphone lifecycle
//! (#323: the microphone is open only while recording).
//!
//! The sink calls [`RecordingCapture::open_for_recording`] right after a
//! recording starts and [`RecordingCapture::close_for_recording`] when the
//! recording ends (after `release_tail_ms`, before transcription) or is
//! cancelled. Between recordings no capture stream exists. The stub session
//! and most sink tests pass `None`.

use std::sync::Arc;

/// Opens and closes the capture device around one recording.
pub(crate) trait RecordingCapture: Send + Sync + 'static {
    /// Open the capture device for the recording that just started. Must
    /// never wait on the session mutex; frames are forwarded without
    /// blocking.
    fn open_for_recording(&self);

    /// Close the capture device. Returns only after every frame the stream
    /// delivered has been handed to the session, so the caller can start
    /// transcription immediately afterwards.
    fn close_for_recording(&self);
}

/// Shared handle the action sink keeps for its lifetime.
pub(crate) type RecordingCaptureHandle = Arc<dyn RecordingCapture>;

pub(super) fn open(capture: Option<&RecordingCaptureHandle>) {
    if let Some(capture) = capture {
        capture.open_for_recording();
    }
}

pub(super) fn close(capture: Option<&RecordingCaptureHandle>) {
    if let Some(capture) = capture {
        capture.close_for_recording();
    }
}
