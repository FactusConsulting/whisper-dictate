//! Backend seam the preview worker calls once per tick, plus its error
//! type. Split out of the pre-1000-LOC single-file `preview.rs` (Codex
//! P1 #608 preview.rs:457 modularity fix) -- kept in its own module so
//! the trait / error definitions are readable at a glance and callers
//! that only need the seam (`crate::dictate::backends::whisper_local`)
//! do not have to skim the engine / state / emission code to find them.

/// Errors a [`PreviewBackend::transcribe_partial`] call can surface.
/// Non-fatal: the worker logs at most once per session and continues.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    /// Underlying model invocation failed.
    #[error("preview backend error: {0}")]
    Backend(String),
}

/// The cheap partial-transcribe seam the preview worker calls once per tick.
///
/// A production impl (see `crate::dictate::backends::WhisperLocalTranscribeBackend`
/// behind the `whisper-rs-local` cargo feature) shares its model instance with
/// the session's final-pass [`crate::dictate::TranscribeBackend`] so the
/// preview does not double resident memory. `Send + Sync` because the trait
/// object lives inside the worker thread AND the session may hold its own
/// clone.
pub trait PreviewBackend: Send + Sync {
    /// Run a partial transcription on `pcm` at `sample_rate`. Returns the
    /// decoded text (may be empty; treated as "nothing to show"). Called at
    /// most once per interval; failures are swallowed by the worker.
    fn transcribe_partial(&self, pcm: &[f32], sample_rate: u32) -> Result<String, PreviewError>;
}
