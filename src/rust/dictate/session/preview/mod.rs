//! Live partial-transcription preview for [`super::DictateSession`].
//!
//! Rust port of `src/python/whisper_dictate/vp_preview.py` -- closes parity
//! blocker #4 on the engine assessment. While the user is still holding PTT,
//! a background worker periodically re-transcribes the accumulated buffer and
//! emits a `state="preview"` worker event so the UI's live pipeline card can
//! show the sentence growing. Strictly DISPLAY-ONLY: the preview never feeds
//! back into the final transcription, never touches dictionary /
//! post-processing / injection / history, and swallows its own errors so a
//! preview failure can never take the session down.
//!
//! # Cadence + gates (parity with `vp_preview.py`)
//!
//! * **Interval** -- fires every `preview_seconds` seconds
//!   (`VOICEPI_PREVIEW_SECONDS`, default `3`). `0` disables.
//! * **Fresh-audio gate** -- a tick that has fewer than
//!   [`MIN_NEW_AUDIO_S`] seconds of NEW audio since the previous
//!   preview is skipped, so short pauses do not re-transcribe an
//!   essentially unchanged buffer.
//! * **Sliding window** -- each tick decodes only the most recent
//!   [`PREVIEW_MAX_AUDIO_S`] seconds so cost is bounded on long
//!   utterances (Python's `PREVIEW_MAX_AUDIO_S` comment: O(n) per
//!   tick, O(n^2) over utterance, unbounded on CPU without this cap).
//! * **Text cap** -- emitted `text_preview` is truncated to
//!   [`PREVIEW_TEXT_CHARS`] chars (generous, wraps in the UI).
//! * **Non-blocking** -- a tick that overruns the interval is skipped,
//!   not queued; the next tick recomputes from the fresh buffer.
//! * **Stop wins** -- once the session calls
//!   [`PreviewEngine::notify_stop`] the worker drops the buffer and
//!   emits NO further previews until the next [`PreviewEngine::notify_start`],
//!   so the final-pass path can never race a stale preview event onto
//!   the wire. Enforced by an atomic flag set synchronously in
//!   `notify_stop` and checked AFTER `transcribe_partial` returns, so a
//!   stop that arrives mid-transcribe suppresses the pending emission
//!   even before the worker consumes the `Stop` message from its
//!   channel (Codex P1 #608 preview.rs:245 — stop-race fix).
//!
//! # Eligibility
//!
//! Only the LOCAL Whisper backend is preview-eligible (`PREVIEW_BACKENDS = ("whisper",)`
//! in `vp_preview.py`). The cloud (`stt_backend=openai`) backend is excluded
//! -- previews there would spam a paid API. The gate is enforced by NOT
//! wiring a [`PreviewEngine`] into the session on the cloud path (see
//! [`crate::runtime::rust_session_real_backends::make_real_session`]).
//!
//! # Thread model
//!
//! One long-lived worker thread per session, driven by an [`std::sync::mpsc::channel`].
//! The session sends `PreviewMsg::Start` on PTT press, `PreviewMsg::Frame`
//! per captured chunk, `PreviewMsg::Stop` on PTT release / cancel; the
//! [`PreviewEngine`]'s `Drop` impl sends `PreviewMsg::Shutdown` and joins.
//! The worker owns its own accumulator buffer -- the session's frame buffer
//! is untouched -- so the audio hot path pays only one channel send per
//! frame (bounded allocation) and never blocks on preview transcribe cost.
//!
//! # Module layout (Codex P1 #608 preview.rs:457 — modularity split)
//!
//! The pre-split single-file `preview.rs` grew past the AGENTS.md 500-LOC
//! modularity limit. It has been extracted into four submodules by
//! responsibility, with this `mod.rs` re-exporting the same public API so
//! every caller keeps using `crate::dictate::session::preview::{...}`:
//!
//! - [`backend`]  -- [`PreviewBackend`] trait + [`PreviewError`] enum.
//! - [`emission`] -- [`PreviewEmission`], [`PreviewSink`],
//!   [`stderr_preview_sink`], [`build_preview_status`] plus the private
//!   `truncate_chars` / `round2` helpers.
//! - [`engine`]   -- [`PreviewEngine`], [`PreviewEngineConfig`],
//!   `PreviewState`, the worker loop, and the tick helper.
//! - [`tests`]    -- all preview tests (cfg(test)).

pub(crate) mod backend;
pub(crate) mod emission;
pub(crate) mod engine;

#[cfg(test)]
mod tests;

pub use backend::{PreviewBackend, PreviewError};
pub use emission::{build_preview_status, stderr_preview_sink, PreviewEmission, PreviewSink};
pub use engine::{
    PreviewEngine, PreviewEngineConfig, MIN_NEW_AUDIO_S, PREVIEW_MAX_AUDIO_S, PREVIEW_TEXT_CHARS,
};
