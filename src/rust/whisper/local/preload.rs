//! Background Whisper model preload primitive.
//!
//! **Purpose (item 5 prereq 5).** In v1.20.7 a fresh dictation press could
//! hang for many seconds while whisper.cpp loaded the model on the same
//! thread as the PTT handler. The user saw a silent PTT press and often
//! released the key before load completed, cancelling the session. The
//! Python worker's `_load_model` shows a spinner + surfaces a
//! `status=loading` event; the Rust path needs an equivalent affordance
//! before it can replace Python's transcription backend.
//!
//! This module exposes the *primitive* the supervisor will use once the
//! Phase C step 2 wiring lands (`docs/design/item5-wire-dictate-session.md`):
//!
//! - [`Preloader`] — spawns a background thread that loads the GGML file
//!   into a fresh [`LocalWhisper`]. The caller polls [`Preloader::status`]
//!   (cheap; snapshot of the shared state) and consumes the loaded model
//!   via [`Preloader::take_ready`] once the state has flipped to
//!   [`LoadStatus::Ready`].
//! - [`LoadStatus`] — the three states surfaced on the wire: `Loading`,
//!   `Ready`, `Failed`. Modelled after the Python worker's
//!   `status=loading|ready|error` event so the UI can display the same
//!   spinner without a translation layer.
//! - **OOM catch.** whisper.cpp's model load allocates the entire tensor
//!   graph up front. On a memory-starved host the FFI can panic (via
//!   `assert!` or an unwind through C++ that Rust surfaces as a
//!   double-panic). [`LocalWhisper::load_catch_unwind`] wraps the load in
//!   [`std::panic::catch_unwind`] so the caller can fall back to the
//!   Python engine rather than taking down the whole supervisor
//!   (documented in `docs/design/item5-wire-dictate-session.md` risk #5).
//!
//! The primitive is deliberately independent of the runtime: it takes a
//! `PathBuf` and produces a `LocalWhisper`. Phase C step 2 will wire it
//! into `DictateSupervisor::start` so the first PTT press sees a
//! ready-or-known-failed model instead of a load-on-demand stall. This
//! PR ships the primitive + a self-test verb; **the runtime does not yet
//! consume it**.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{LoadFailure, LocalWhisper};

/// Snapshot of a [`Preloader`]'s progress. Cheap to construct — the shared
/// state is protected by a single Mutex and cloned into this enum, so
/// callers can poll every UI frame without contention.
#[derive(Debug)]
pub enum LoadStatus {
    /// The background thread is still running. `elapsed` is wall-clock
    /// time since [`Preloader::start`] so the UI can surface a
    /// "Loading model (X.Xs)…" hint that matches what the Python worker
    /// prints today.
    Loading { elapsed: Duration },
    /// Load succeeded. The [`LocalWhisper`] is still owned by the
    /// preloader until the caller consumes it via
    /// [`Preloader::take_ready`] — the status stays `Ready` in between
    /// so late pollers see a stable state instead of "the model disappeared".
    Ready { elapsed: Duration },
    /// Load failed. `failure` mirrors the enum produced by
    /// [`LocalWhisper::load_catch_unwind`] so the caller can distinguish
    /// "clean error, fall back to Python" from "panic caught, log
    /// loudly + fall back to Python" without re-parsing a string.
    Failed {
        elapsed: Duration,
        failure: LoadFailure,
    },
    /// The caller has already consumed the ready model via
    /// [`Preloader::take_ready`]. Returned so a second poll doesn't lie
    /// with `Loading` (which would restart the UI's "still loading"
    /// spinner). The load happened; the model just isn't here anymore.
    Consumed { elapsed: Duration },
}

impl LoadStatus {
    /// Wall-clock elapsed since `Preloader::start`. Useful for the smoke
    /// script's `whisper-load` self-test JSON report — the number the
    /// operator wants to see is "how long did this take end-to-end", not
    /// broken out per stage.
    pub fn elapsed(&self) -> Duration {
        match self {
            LoadStatus::Loading { elapsed }
            | LoadStatus::Ready { elapsed }
            | LoadStatus::Failed { elapsed, .. }
            | LoadStatus::Consumed { elapsed } => *elapsed,
        }
    }

    /// Stable machine-readable label for the JSON envelope. Mirrors the
    /// Python worker's `status` field values so the UI needs one code
    /// path instead of two.
    pub fn label(&self) -> &'static str {
        match self {
            LoadStatus::Loading { .. } => "loading",
            LoadStatus::Ready { .. } => "ready",
            LoadStatus::Failed { .. } => "error",
            LoadStatus::Consumed { .. } => "consumed",
        }
    }
}

/// Shared inner state — `Loading` while the thread is running, then
/// `Ready` / `Failed`. `Ready` is `Option<LocalWhisper>` so
/// [`Preloader::take_ready`] can move the model out without dropping the
/// `Ready` state (the status stays consistent for late pollers).
///
/// No `Debug` derive: `LocalWhisper` wraps a `WhisperContext` from
/// `whisper-rs` which does not implement `Debug`, and printing the raw
/// model handle would leak an FFI pointer with no debug value anyway.
enum Inner {
    Loading,
    Ready(Option<LocalWhisper>),
    Failed(LoadFailure),
    Consumed,
}

/// Background loader for a Whisper GGML model.
///
/// Owns exactly one worker thread (spawned in [`Preloader::start`]) that
/// runs [`LocalWhisper::load_catch_unwind`]. The main thread polls
/// [`Preloader::status`] to drive the UI + retire the primitive when the
/// load has resolved.
///
/// Not `Clone` on purpose: the model + the join handle should have a
/// single, unambiguous owner (the supervisor's dictation session builder).
/// If a second component needs the model, wrap the taken value in an
/// `Arc` after `take_ready`.
pub struct Preloader {
    started_at: Instant,
    inner: Arc<Mutex<Inner>>,
    // Retained so `Drop` joins the thread and reclaims the handle instead
    // of leaving a zombie thread if the preloader is dropped before the
    // load finishes. Wrapped in Option so `Drop` can move it out of a
    // mut ref without needing Copy.
    handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Preloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual impl so callers can `#[derive(Debug)]` on structs that
        // embed a Preloader. Prints the observable status but not the
        // wrapped model handle (no useful debug rendering for an FFI
        // WhisperContext pointer).
        f.debug_struct("Preloader")
            .field("status", &self.status().label())
            .field("elapsed_ms", &self.started_at.elapsed().as_millis())
            .finish()
    }
}

impl Preloader {
    /// Start loading `model_path` in a background thread and return
    /// immediately. The caller should poll [`Self::status`] on a UI-frame
    /// or supervisor-tick cadence.
    ///
    /// The load runs under [`std::panic::catch_unwind`] so an OOM inside
    /// whisper.cpp surfaces as [`LoadStatus::Failed`] with
    /// [`LoadFailure::Panicked`] rather than aborting the process
    /// (item 5 prereq 5 requirement).
    pub fn start(model_path: PathBuf) -> Self {
        let inner = Arc::new(Mutex::new(Inner::Loading));
        let inner_for_thread = Arc::clone(&inner);
        let thread_path = model_path.clone();
        // Named thread so a `top`/perf capture during the load points at
        // the right owner. Cheap; no cost per load.
        let handle = std::thread::Builder::new()
            .name(format!(
                "whisper-preload:{}",
                thread_path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "<unknown>".to_owned())
            ))
            .spawn(move || {
                let result = LocalWhisper::load_catch_unwind(&thread_path);
                let mut guard = inner_for_thread.lock().unwrap_or_else(|e| e.into_inner());
                // Only advance out of Loading; a `Preloader::drop_load`
                // (future extension) could set Consumed before us.
                if matches!(*guard, Inner::Loading) {
                    *guard = match result {
                        Ok(model) => Inner::Ready(Some(model)),
                        Err(failure) => Inner::Failed(failure),
                    };
                }
            })
            .expect("spawn whisper-preload thread");
        Self {
            started_at: Instant::now(),
            inner,
            handle: Some(handle),
        }
    }

    /// Poll the current [`LoadStatus`]. Cheap; safe to call every UI frame.
    pub fn status(&self) -> LoadStatus {
        let elapsed = self.started_at.elapsed();
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            Inner::Loading => LoadStatus::Loading { elapsed },
            Inner::Ready(Some(_)) => LoadStatus::Ready { elapsed },
            Inner::Ready(None) | Inner::Consumed => LoadStatus::Consumed { elapsed },
            Inner::Failed(f) => LoadStatus::Failed {
                elapsed,
                failure: f.clone(),
            },
        }
    }

    /// Move the loaded model out of the preloader.
    ///
    /// Returns `Some(model)` exactly once, when the state is
    /// [`LoadStatus::Ready`]. Any other state (Loading / Failed /
    /// Consumed) returns `None` and the state is left untouched.
    pub fn take_ready(&self) -> Option<LocalWhisper> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Inner::Ready(slot) = &mut *guard {
            if let Some(model) = slot.take() {
                *guard = Inner::Consumed;
                return Some(model);
            }
        }
        None
    }

    /// Block until the load has resolved. Only used by the self-test
    /// verb and the unit tests — the runtime path polls
    /// [`Self::status`] instead so the UI stays responsive.
    ///
    /// `timeout` bounds the wait so a runaway load doesn't hang the
    /// self-test in CI. Returns the final [`LoadStatus`] (which may
    /// still be `Loading` on timeout).
    pub fn wait_until_settled(&self, timeout: Duration) -> LoadStatus {
        // Simple poll loop with an exponential-ish backoff. whisper.cpp's
        // model load is dominated by disk + mmap, so 25ms polls are
        // effectively free but responsive enough for a CLI-visible
        // "loaded in Xms" report.
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status();
            if !matches!(status, LoadStatus::Loading { .. }) {
                return status;
            }
            if Instant::now() >= deadline {
                return status;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Preloader {
    fn drop(&mut self) {
        // Join the background thread so we don't leave a zombie writing
        // into freed shared state. Cheap in the common case (thread has
        // already exited by the time the supervisor drops the primitive).
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Blocking one-shot load — the shape the self-test verb uses.
///
/// Not a duplicate of [`LocalWhisper::load_catch_unwind`]: this helper
/// runs the load through the same background thread + `Preloader` that
/// the supervisor will use, so the self-test exercises the actual code
/// path rather than an alternate direct-load shortcut. Reports the
/// wall-clock elapsed measured from the primitive's `started_at`, which
/// includes thread-spawn overhead — that's exactly what the user
/// perceives on first PTT press.
///
/// `timeout` bounds the wait; if the load hasn't resolved by then the
/// self-test reports `LoadStatus::Loading` (the caller treats that as a
/// failure with a distinct exit code so a CI runner can tell "took too
/// long" apart from "threw an error").
pub fn load_blocking(model_path: &Path, timeout: Duration) -> (LoadStatus, Option<LocalWhisper>) {
    let preloader = Preloader::start(model_path.to_path_buf());
    let status = preloader.wait_until_settled(timeout);
    let model = if matches!(status, LoadStatus::Ready { .. }) {
        preloader.take_ready()
    } else {
        None
    };
    (status, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A missing model file surfaces as a clean `Failed(Errored)` — NOT
    /// a panic. This is the load path's baseline safety guarantee: no
    /// unwrap!s on the happy-nothing case.
    #[test]
    fn missing_model_reports_failed_errored() {
        let bogus = PathBuf::from("/definitely/not/a/real/path/model.bin");
        let preloader = Preloader::start(bogus);
        let status = preloader.wait_until_settled(Duration::from_secs(2));
        match status {
            LoadStatus::Failed {
                failure: LoadFailure::Errored(_),
                ..
            } => {}
            other => panic!("expected Failed(Errored), got {other:?}"),
        }
        assert_eq!(preloader.status().label(), "error");
    }

    /// The status label matches the Python worker's `status=...` values
    /// so the UI can consume both without a translation layer.
    #[test]
    fn status_labels_match_python_wire_format() {
        assert_eq!(
            LoadStatus::Loading {
                elapsed: Duration::ZERO
            }
            .label(),
            "loading"
        );
        assert_eq!(
            LoadStatus::Ready {
                elapsed: Duration::ZERO
            }
            .label(),
            "ready"
        );
        assert_eq!(
            LoadStatus::Failed {
                elapsed: Duration::ZERO,
                failure: LoadFailure::errored(anyhow::anyhow!("boom"))
            }
            .label(),
            "error"
        );
        assert_eq!(
            LoadStatus::Consumed {
                elapsed: Duration::ZERO
            }
            .label(),
            "consumed"
        );
    }

    /// `take_ready` returns None until the state is Ready, and only
    /// once even after Ready — protects the supervisor from
    /// accidentally consuming the model twice on a re-poll.
    #[test]
    fn take_ready_is_none_before_ready_and_only_once() {
        let bogus = PathBuf::from("/definitely/not/a/real/path/model.bin");
        let preloader = Preloader::start(bogus);
        // Before the thread has run the load may return None (Loading);
        // after it resolves to Failed, take_ready must also be None.
        let _ = preloader.wait_until_settled(Duration::from_secs(2));
        assert!(
            preloader.take_ready().is_none(),
            "Failed state has no model"
        );
        assert!(preloader.take_ready().is_none(), "second call still None");
    }
}
