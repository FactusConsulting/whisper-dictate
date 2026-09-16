//! Seam between the coordinator action sink and the microphone lifecycle
//! (#323: the microphone is open only while recording).
//!
//! The recording actions call [`RecordingCapture::open_for_recording`] after
//! a recording begins and announce the recording only when it succeeded, and
//! call [`RecordingCapture::close_for_recording`] when the recording ends
//! (after `release_tail_ms`, before transcription) or is cancelled. Between
//! recordings no capture stream exists. The stub session and most sink tests
//! pass `None`.

use std::sync::Arc;

/// Opens and closes the capture device around one recording.
pub(crate) trait RecordingCapture: Send + Sync + 'static {
    /// Open the capture device for the recording that just began. Returns
    /// `false` when no stream could be opened; the failure has already been
    /// reported and the caller must not announce the recording. Must never
    /// wait on the session mutex.
    fn open_for_recording(&self) -> bool;

    /// Close the capture device. Returns only after every frame the stream
    /// delivered has been handed to the session, so the caller can start
    /// transcription immediately afterwards.
    fn close_for_recording(&self);
}

/// Shared handle the action sink keeps for its lifetime.
pub(crate) type RecordingCaptureHandle = Arc<dyn RecordingCapture>;

/// `true` when there is nothing to open (stub sessions) or the open worked.
pub(super) fn open(capture: Option<&RecordingCaptureHandle>) -> bool {
    capture.is_none_or(|capture| capture.open_for_recording())
}

pub(super) fn close(capture: Option<&RecordingCaptureHandle>) {
    if let Some(capture) = capture {
        capture.close_for_recording();
    }
}

/// Closes the capture when the enclosing action unwinds from a panic before
/// its explicit close, so a panic (for example in a live-settings reload)
/// can never leave the microphone open until runtime teardown.
pub(super) struct CloseOnUnwind<'a> {
    capture: Option<&'a RecordingCaptureHandle>,
}

pub(super) fn close_on_unwind(capture: Option<&RecordingCaptureHandle>) -> CloseOnUnwind<'_> {
    CloseOnUnwind { capture }
}

impl Drop for CloseOnUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            close(self.capture);
        }
    }
}

#[cfg(test)]
#[path = "recording_capture_tests.rs"]
mod tests;
