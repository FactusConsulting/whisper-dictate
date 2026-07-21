//! Runtime local-vs-cloud transcribe selection for the in-process session.
//!
//! The Python worker honours `stt_backend` (`local` Whisper vs the cloud
//! `openai`/Groq `/audio/transcriptions` endpoint). The in-process Rust
//! session ([`crate::runtime::rust_session_real_backends::make_real_session`])
//! only ever built the local Whisper backend, so a user who saved
//! `stt_backend=openai` silently got local inference (or an error when no
//! model was installed). This enum closes that gap: it wraps the two
//! [`TranscribeBackend`] impls behind one type so `make_real_session` can
//! pick the variant at construction from `VOICEPI_STT_BACKEND` -- exactly
//! the same env key the worker command exports.
//!
//! # Why an enum, not `Box<dyn TranscribeBackend>`
//!
//! [`crate::dictate::DictateSession`] is generic over its transcribe
//! backend `T`. A single concrete `T` that can be *either* backend is the
//! cheapest way to keep the session monomorphic (no per-utterance vtable
//! indirection) while letting the runtime choose. It also mirrors the
//! sibling [`crate::runtime::rust_session_inject::ProductionInjectBackend`]
//! enum, which already does the same for the inject seam.
//!
//! # Why generic over the local backend `L`
//!
//! The local variant is [`crate::dictate::backends::WhisperLocalTranscribeBackend`],
//! which only exists behind the `whisper-rs-local` cargo feature. Making
//! the enum generic over `L` keeps this module **stock** (no feature gate):
//! it compiles and is unit-tested on every build with a stub `L`, and the
//! feature-gated `make_real_session` merely binds
//! `L = WhisperLocalTranscribeBackend`. The cloud variant is always
//! available because [`CloudTranscribeBackend`] is stock.

use crate::dictate::backends::CloudTranscribeBackend;
use crate::dictate::{TranscribeBackend, TranscribeError, TranscribeResult};

/// The transcribe backend the in-process session runs, chosen at
/// construction: local Whisper (`L`) or the cloud
/// (`openai`/Groq) endpoint.
pub enum ProductionTranscribeBackend<L> {
    /// Local Whisper inference (`stt_backend=local`, the default).
    Local(L),
    /// Cloud `/audio/transcriptions` endpoint (`stt_backend=openai`).
    Cloud(CloudTranscribeBackend),
}

impl<L> ProductionTranscribeBackend<L> {
    /// Choose the transcribe backend from the operator's `stt_backend`
    /// selection, deferring the EXPENSIVE construction of each arm behind a
    /// thunk so only the selected arm runs.
    ///
    /// `cloud_requested` is
    /// [`crate::dictate::backends::cloud_transcribe::cloud_backend_requested_from_env`]'s
    /// verdict. Critically, on the cloud path `build_local` is **never
    /// called** -- so a cloud user pays no local model-path / idle-timeout
    /// resolution (and needs no GGML model installed at all). `build_local`
    /// may fail (model resolution); `build_cloud` is infallible. Kept
    /// generic over `L` + the error type so it is unit-testable in a stock
    /// build with a stub local backend, independent of the feature-gated
    /// [`crate::dictate::backends::WhisperLocalTranscribeBackend`] the
    /// production caller binds.
    pub fn select<E>(
        cloud_requested: bool,
        build_cloud: impl FnOnce() -> CloudTranscribeBackend,
        build_local: impl FnOnce() -> Result<L, E>,
    ) -> Result<Self, E> {
        if cloud_requested {
            Ok(Self::Cloud(build_cloud()))
        } else {
            Ok(Self::Local(build_local()?))
        }
    }
}

impl<L: TranscribeBackend> TranscribeBackend for ProductionTranscribeBackend<L> {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        match self {
            Self::Local(backend) => backend.transcribe(pcm, sample_rate),
            Self::Cloud(backend) => backend.transcribe(pcm, sample_rate),
        }
    }
}

#[cfg(test)]
#[path = "production_transcribe_tests.rs"]
mod tests;
