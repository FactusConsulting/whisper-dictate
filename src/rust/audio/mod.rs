//! Native push-to-talk audio capture (cpal -> resampler -> 16 kHz frames).
//!
//! The `audio-capture` feature owns microphone capture, device probing,
//! resampling, and the VAD-free [`raw::RawCapturePipeline`]. Push-to-talk
//! already supplies the utterance boundaries, so the production path does not
//! perform voice-activity detection or load a model before opening the mic.

pub(crate) mod bounded_queue;
pub mod capture;
pub mod device_probe;
pub mod hosts;
pub mod pipewire;
pub mod raw;
pub mod resampler;
pub mod self_test;

pub use capture::{AudioChunk, CaptureHandle};
pub use raw::{PipelineReceiver, RawCaptureOverflow, RawCapturePipeline};
pub use resampler::{FrameResampler, FRAME_SIZE, OUTPUT_RATE};

/// Events emitted by the push-to-talk capture pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineEvent {
    /// One resampled 16 kHz frame.
    Frame(Vec<f32>),
    /// Capture failed unrecoverably. No events follow this terminal event.
    DeviceError(String),
}
