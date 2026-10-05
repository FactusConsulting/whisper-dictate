//! Windows GUI mode shortcuts (cycle / raw / clean). The hotkey listener
//! only queues work here; the config read-modify-write runs on a dedicated
//! serialized worker thread.
//!
//! Unlike copy-last and paste-last these shortcuts never touch the keyboard
//! or the clipboard — they only persist a new `post_mode` value. That means
//! no suppression while a recording is active and no restore coordination
//! with the injection backends. The persistence is asynchronous: a
//! transcription pass that reloads `post_mode` before the worker's write
//! lands keeps the previous mode, and a pass that starts afterwards uses
//! the new one.
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

use super::{RuntimeEvent, WorkerEvent};
use crate::postprocess::POST_MODE_ENV;

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
/// caller (the OS hotkey listener thread). If the worker thread is gone
/// (its spawn failed, or the channel disconnected) the press reports a
/// runtime error instead of silently doing nothing.
pub(super) fn queue(
    tx: Sender<RuntimeEvent>,
    request: ModeRequest,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) {
    let job = ModeJob {
        request,
        tx: tx.clone(),
        repaint_notifier: repaint_notifier.clone(),
    };
    if MODE_JOBS.send(job).is_err() {
        let _ = tx.send(RuntimeEvent::Stderr(
            "[hotkey] mode shortcut worker is unavailable; the mode change was not applied"
                .to_owned(),
        ));
        if let Some(notifier) = repaint_notifier.as_ref() {
            notifier();
        }
    }
}

/// Apply one queued job: persist the new mode and report the outcome.
fn apply_job(job: ModeJob) {
    for outcome in run(job.request) {
        let _ = job.tx.send(outcome);
    }
    if let Some(notifier) = job.repaint_notifier.as_ref() {
        notifier();
    }
}

/// Resolve the request against the current config and persist it. Returns
/// the events the runtime should emit for the press: a structured
/// `post_mode_changed` worker event the Settings page reconciles its
/// snapshots from, plus the human-readable log line (or an error).
/// Synchronous so tests can run it directly under the env lock.
fn run(request: ModeRequest) -> Vec<RuntimeEvent> {
    // Codex P2 mode_shortcuts.rs:140: hold CONFIG_WRITE_LOCK across
    // resolving AND persisting the press so a concurrent Settings save
    // cannot slot a new post_mode (or other keys) between this read and
    // this write. The lock helper tolerates poisoning, and the setter
    // below is the under-lock variant to avoid re-locking.
    let _guard = crate::config::config_write_guard();
    // Resolve the current mode with the same precedence the live runtime
    // uses when it builds the worker environment: the on-disk config,
    // then the process environment, then the schema default
    // (`effective_runtime_env`). A VOICEPI_POST_MODE the runtime honours
    // must not be ignored just because config.json has no post_mode yet.
    let env = crate::config::effective_runtime_env();
    let current = match env.get(POST_MODE_ENV) {
        Some(mode) => mode.clone(),
        None => match crate::config::load_settings() {
            Ok(settings) => settings.post_mode,
            Err(error) => {
                return vec![RuntimeEvent::Stderr(format!(
                    "[hotkey] could not read settings for the mode shortcut: {}",
                    crate::diag::ascii_escaped(&error.to_string())
                ))];
            }
        },
    };
    // Codex P2 mode_shortcuts.rs:152: the live postprocessor normalizes
    // aliases (bullet-list -> bullets) and case (EMAIL -> email) before
    // every dictation, so cycle from the normalized form; otherwise a
    // non-canonical VOICEPI_POST_MODE would be treated as unknown.
    let current = crate::postprocess::normalize_mode(&current);
    let mode = resolve_post_mode(request, &current);
    // Write ONLY the post_mode key: a whole-snapshot save would
    // materialize defaults into a sparse config.json and override the
    // environment fallbacks the user relies on (Codex P1
    // mode_shortcuts.rs:129). The under-lock setter merges the single
    // key into the existing file and writes it atomically.
    if let Err(error) = crate::config::set_raw_string_key_under_lock(
        "post_mode",
        &mode,
        &crate::config::config_path(),
    ) {
        return vec![RuntimeEvent::Stderr(format!(
            "[hotkey] could not save post mode: {}",
            crate::diag::ascii_escaped(&error.to_string())
        ))];
    }
    vec![
        RuntimeEvent::Worker(WorkerEvent {
            event: "post_mode_changed".to_owned(),
            state: None,
            payload: serde_json::json!({ "mode": mode }),
        }),
        RuntimeEvent::Stdout(format!("[hotkey] post mode: {mode}")),
    ]
}
