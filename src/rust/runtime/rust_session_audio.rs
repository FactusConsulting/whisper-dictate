//! Production wiring of the push-to-talk microphone into the real
//! [`crate::dictate::DictateSession`] (#323).
//!
//! The capture device is open ONLY while recording: it opens when a
//! recording starts (push-to-talk press or toggle-on) and closes when the
//! recording ends, after `release_tail_ms` and before transcription. Runtime
//! start only validates the configured input by enumeration.
//!
//! Before #323 this module opened a VAD-free [`RawCapturePipeline`] as soon as
//! the runtime started and discarded frames while idle, which kept the OS
//! microphone indicator on permanently. The lifecycle now lives in
//! [`super::capture_lifecycle`] (open/close points), with device selection in
//! [`super::capture_open_policy`] and frame delivery in
//! [`super::capture_forwarder`]; this module only supplies the CPAL opener.
//!
//! # Gating
//!
//! Compiled in only when ALL THREE features are on:
//!
//! * `whisper-rs-local` -- the parent
//!   [`super::rust_session_real_backends`] module is gated on this.
//! * `rust-injection` -- same.
//! * `audio-capture` -- provides [`RawCapturePipeline`].

#![cfg(feature = "audio-capture")]

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use super::capture_forwarder::SessionFrameSink;
use super::capture_lifecycle::{CaptureLifecycle, CaptureOpener};
use super::capture_status::CaptureReporter;
use super::recording_capture::RecordingCaptureHandle;
use super::supervisor::CaptureStop;
use crate::audio::{PipelineReceiver, RawCapturePipeline};
use crate::dictate::session::{DictateSession, InjectBackend, TranscribeBackend};
use crate::runtime::{RepaintNotifier, RuntimeEvent};

/// Opens real CPAL capture. Dropping the returned pipeline stops the stream.
pub(crate) struct RawCaptureOpener;

impl CaptureOpener for RawCaptureOpener {
    type Stream = RawCapturePipeline;

    fn probe(&self, selector: &str) -> Result<(), anyhow::Error> {
        // Enumeration only: resolving a device never starts a stream, so the
        // microphone-in-use indicator stays off.
        crate::audio::hosts::resolve_input(selector).map(|_| ())
    }

    fn open(
        &self,
        selector: &str,
    ) -> Result<(RawCapturePipeline, PipelineReceiver), anyhow::Error> {
        RawCapturePipeline::start(selector)
    }

    fn is_timeout(&self, error: &anyhow::Error) -> bool {
        crate::audio::capture::is_capture_start_timeout(error)
    }
}

/// Handles the real session keeps for the runtime's lifetime.
pub(crate) struct SessionAudio {
    /// Opened/closed by the coordinator action sink around each recording.
    pub(crate) recording_capture: RecordingCaptureHandle,
    /// Teardown callback; closes an open stream without waiting for
    /// transcription.
    pub(crate) capture_stop: CaptureStop,
}

impl SessionAudio {
    /// Check `device` without opening it and build the lifecycle that feeds
    /// `session` while recording. Device statuses (including "no microphone
    /// found") go to `tx`; construction never fails.
    pub(crate) fn for_session<T, I>(
        session: Arc<Mutex<DictateSession<T, I>>>,
        tx: Sender<RuntimeEvent>,
        repaint_notifier: Option<RepaintNotifier>,
        device: &str,
    ) -> Self
    where
        T: TranscribeBackend + Send + 'static,
        I: InjectBackend + Send + 'static,
    {
        let effective_audio_device = session
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .effective_audio_device_handle();
        let reporter = CaptureReporter::new(tx, repaint_notifier, effective_audio_device);
        let frames = Arc::new(SessionFrameSink::new(session));
        let lifecycle = CaptureLifecycle::new(RawCaptureOpener, frames, device, reporter);
        let capture_stop = lifecycle.capture_stop();
        Self {
            recording_capture: Arc::new(lifecycle),
            capture_stop,
        }
    }
}

#[cfg(test)]
#[path = "rust_session_audio_tests.rs"]
mod tests;
