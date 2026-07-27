//! Round 2/3 backend self-tests for the Rust dictation engine.
//!
//! Every module in this directory implements ONE `whisper-dictate self-test
//! <verb>` for one of the six Round 2/3 backends (feedback cues, WASAPI
//! audio ducking, profile matching, history JSONL sink, metrics JSONL sink,
//! live preview). The design goal is the same as the existing
//! `hotkey::self_test` / `injection::self_test` / `audio::self_test`
//! siblings: each verb is pure enough to run on any headless dev box
//! (Windows CI container, Ubuntu smoke box, an SSH terminal), returns a
//! machine-readable JSON envelope the `wayland-user-smoke.sh` script can
//! pin, and exits non-zero on any observable regression so CI trips.
//!
//! ## Verb roster
//!
//! * [`feedback`] — plays start + stop cue via [`crate::dictate::feedback`],
//!   reports which backend fired.
//! * [`audio_ducking`] — enters + exits the ducker, reports the resolved
//!   backend / level / before-after state.
//! * [`profile_match`] — runs the [`crate::dictate::profile`] matcher
//!   against a synthetic `WindowInfo` and reports the resolved
//!   [`crate::dictate::AppliedProfile`].
//! * [`history_write`] — writes one utterance event through
//!   [`crate::dictate::session::history_sink_from_settings`] and reports the
//!   file path + row written.
//! * [`metrics_write`] — same shape as `history_write` but for the metrics
//!   sink.
//! * [`preview`] — spins up a [`crate::dictate::PreviewEngine`] with a mock
//!   backend, drives `push_frame` N times, and reports the collected
//!   emissions.
//!
//! ## Report envelope
//!
//! Every verb ultimately produces a `serde_json::Value` printed as a single
//! line. The stable outer keys are:
//!
//! ```json
//! {
//!   "kind": "<verb-token>_self_test",
//!   "ok": true|false,
//!   "error": null | "…",
//!   … verb-specific fields …
//! }
//! ```
//!
//! `ok=false` triggers a non-zero exit from the CLI dispatcher in
//! `main.rs::handle_self_test`; the operator-facing error message lives on
//! the `error` field so the smoke script's `grep` on the message keeps
//! working across verbs.

pub mod audio_ducking;
pub mod feedback;
pub mod history_write;
pub mod metrics_write;
pub mod preview;
pub mod profile_match;

// Sibling regression tests. Each `<verb>_tests.rs` file pins the crate-
// public API surface the CLI dispatcher in `main.rs` calls through. Kept
// as sibling files (rather than only inside each verb's `#[cfg(test)]
// mod tests`) so the regression-test discipline scanner
// (`src/tests/python/test_regression_test_discipline.py`) sees a matching
// test file for every new self-test module added in this PR.
#[cfg(test)]
mod audio_ducking_tests;
#[cfg(test)]
mod feedback_tests;
#[cfg(test)]
mod history_write_tests;
#[cfg(test)]
mod metrics_write_tests;
#[cfg(test)]
mod preview_tests;
#[cfg(test)]
mod profile_match_tests;
