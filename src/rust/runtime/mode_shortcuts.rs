//! Windows GUI mode shortcuts (cycle / raw / clean). The hotkey listener
//! only queues work here; the config read-modify-write runs on a dedicated
//! serialized worker thread.
//!
//! Unlike copy-last and paste-last these shortcuts never touch the keyboard
//! or the clipboard — they only persist a new `post_mode` value. That means
//! no suppression while a recording is active (the in-flight recording's
//! transcription pass reads `post_mode` when it starts, so a mode change
//! made mid-recording simply applies to that recording) and no restore
//! coordination with the injection backends.
//!
//! Serialisation: presses can arrive faster than the config write finishes,
//! and cycling twice must advance the mode twice, so the spawn-per-press
//! busy-flag pattern (which drops concurrent presses) is wrong here. A
//! single long-lived worker drains an mpsc queue instead; each press is a
//! queued job applied in arrival order against a freshly reloaded settings
//! snapshot. The worker re-reads the config path per job, so ambient config
//! overrides behave like every other live reload.

#[cfg(test)]
#[path = "mode_shortcut_tests.rs"]
mod tests;

use std::sync::mpsc::{channel, Sender};
use std::sync::LazyLock;

use super::RuntimeEvent;

/// The post-processing mode choices in schema order. Cycling wraps after
/// the last entry back to raw.
pub(crate) const POST_MODE_CHOICES: [&str; 7] = [
    "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
];

/// Which mode shortcut fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeRequest {
    /// Advance `post_mode` to the next choice (wrapping to raw).
    Cycle,
    /// Set `post_mode` to raw.
    Raw,
    /// Set `post_mode` to clean.
    Clean,
}

/// Return the mode that follows `current` in [`POST_MODE_CHOICES`].
/// Unknown values (a hand-edited config) wrap forward to raw.
fn next_post_mode(current: &str) -> String {
    let current = current.trim();
    match POST_MODE_CHOICES.iter().position(|m| *m == current) {
        Some(index) => POST_MODE_CHOICES[(index + 1) % POST_MODE_CHOICES.len()].to_owned(),
        // An unrecognized value is not part of the cycle, so the first
        // press snaps the mode back onto the nearest sane choice: raw.
        None => POST_MODE_CHOICES[0].to_owned(),
    }
}

/// Resolve `request` against `current` and return the new mode value.
fn resolve_post_mode(request: ModeRequest, current: &str) -> String {
    match request {
        ModeRequest::Cycle => next_post_mode(current),
        ModeRequest::Raw => "raw".to_owned(),
        ModeRequest::Clean => "clean".to_owned(),
    }
}

struct ModeJob {
    request: ModeRequest,
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
}

/// Lazily spawned worker drain. A single thread serialises config writes;
/// the channel is unbounded so a press never blocks the OS listener
/// thread.
static MODE_JOBS: LazyLock<Sender<ModeJob>> = LazyLock::new(|| {
    let (tx, rx) = channel::<ModeJob>();
    // A failed spawn leaves the jobs queued but never applied; there is
    // nothing the listener thread could do about it, and the process is
    // about to exit anyway.
    let _ = std::thread::Builder::new()
        .name("wd-mode-shortcuts".to_owned())
        .spawn(move || {
            for job in rx {
                apply_job(job);
            }
        });
    tx
});

/// Queue a mode shortcut for the serialized worker. Never blocks the
/// caller (the OS hotkey listener thread).
pub(super) fn queue(
    tx: Sender<RuntimeEvent>,
    request: ModeRequest,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) {
    let _ = MODE_JOBS.send(ModeJob {
        request,
        tx,
        repaint_notifier,
    });
}

/// Apply one queued job: persist the new mode and report the outcome.
fn apply_job(job: ModeJob) {
    let outcome = run(job.request);
    let _ = job.tx.send(outcome);
    if let Some(notifier) = job.repaint_notifier.as_ref() {
        notifier();
    }
}

/// Resolve the request against the current config and persist it. Returns
/// the event the runtime should log for the press. Synchronous so tests
/// can run it directly under the env lock.
fn run(request: ModeRequest) -> RuntimeEvent {
    let mut settings = match crate::config::load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            return RuntimeEvent::Stderr(format!(
                "[hotkey] could not read settings for the mode shortcut: {}",
                crate::diag::ascii_escaped(&error.to_string())
            ));
        }
    };
    let mode = resolve_post_mode(request, &settings.post_mode);
    settings.post_mode = mode.clone();
    if let Err(error) = crate::config::save_settings(&settings) {
        return RuntimeEvent::Stderr(format!(
            "[hotkey] could not save post mode: {}",
            crate::diag::ascii_escaped(&error.to_string())
        ));
    }
    RuntimeEvent::Stdout(format!("[hotkey] post mode: {mode}"))
}
