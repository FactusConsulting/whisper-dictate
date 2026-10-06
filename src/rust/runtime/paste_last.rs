//! Windows GUI paste-last action. The hotkey listener only queues work here;
//! history reads and the injection run outside its message loop.
//!
//! Design mirrors [`super::copy_last`] with one deliberate difference: the
//! paste arm builds a fresh injection backend per invocation instead of
//! reusing a shared handle. The shared UI/runtime handles carry a captured
//! reinject target and a clipboard-restore cycle for their own flows, and
//! a paste-last press must neither read nor mutate that state;
//! construction is cheap because `Injector::new` performs no system calls
//! and the enigo backend is built lazily on first use. Three process-wide
//! mechanisms still reach the fresh backend: the keystroke burst is
//! serialized against every other injection path by the pipeline lock in
//! `EnigoInjectBackend::inject_using`, the burst is bracketed through the
//! `inject_guard::global` slot that `install_hotkey` populates, and the
//! paste-arm backend is registered with
//! `injection::ui::register_runtime_backend` so Stop's
//! `cancel_pending_clipboard_restore` and explicit copies'
//! `copy_with_pending_restore_cancelled` reach its restore cycle. The
//! runtime lifecycle flag is forwarded to `with_cancellation_flag` so
//! Stop/Restart terminates an in-flight burst at the next character
//! boundary instead of typing into a window after the UI already reported
//! the runtime stopped. The press handler captures the foreground window
//! on the hotkey listener thread and the worker re-activates that
//! snapshot inside the pipeline lock right before the burst, so a
//! dictation finishing around the same time cannot steal focus and
//! misdirect the paste .
//!
//! The inject mode is resolved at press time with the same precedence the
//! per-utterance live-settings reload applies (configured config.json,
//! then the ambient environment, then the schema default, then the
//! install-time snapshot as a last resort), so flipping Output mode in
//! the settings UI while the runtime stays active behaves consistently
//! instead of honoring the mode captured at hotkey install. The history
//! row's recorded inject mode is honored too: the utterance payload stores
//! the session's EFFECTIVE mode, so a per-window profile override (print
//! for a privacy-sensitive window) still rejects a paste-last press even
//! after the global mode changed — copy-last is the right action for
//! those transcripts.
//!
//! The transcript always comes from local history, which the dictate session
//! records even when its own injection failed (`dictate/session/transcription.rs`
//! records the attempted utterance alongside `inject_error`), so re-pasting a
//! transcript whose original injection failed works without any extra state.
//!
//! A pending dictation paste-restore is never clobbered: the restore timers
//! only rewrite the clipboard when it still holds the text they injected, so
//! an intervening paste-last write retires their restore cycle, and
//! paste-last's own pending restore is registered with the process-wide
//! coordination registry so explicit copies cancel it cleanly.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc};

use super::RuntimeEvent;

/// Env var the settings layer uses for inject mode. Mirrors the hardcoded
/// lookup in `rust_session_real_backends` (the shared const there is
/// test-only).
const INJECT_MODE_ENV: &str = "VOICEPI_INJECT_MODE";

/// Worker seam for tests: production injects through the native backend,
/// tests record the text and mode instead.
trait TextInjector {
    fn paste(
        &mut self,
        text: &str,
        mode: &str,
        cancellation: Option<Arc<AtomicBool>>,
        target: Option<crate::platform::foreground_window::WindowInfo>,
    ) -> anyhow::Result<()>;
}

struct NativeInjector;

impl TextInjector for NativeInjector {
    fn paste(
        &mut self,
        text: &str,
        mode: &str,
        cancellation: Option<Arc<AtomicBool>>,
        target: Option<crate::platform::foreground_window::WindowInfo>,
    ) -> anyhow::Result<()> {
        crate::injection::ui::paste_last_into_focused_window(text, mode, cancellation, target)
    }
}

/// Resolve the effective inject mode for this press, mirroring
/// `live_settings::select_live_value` without the forced-CLI tier (the
/// paste-last worker has no access to that run-scoped map): a configured
/// config.json value wins, then the ambient environment, then the schema
/// default; the install-time snapshot is the last resort so a settings
/// load failure never silently changes behaviour. Blank values fall
/// through to the snapshot, matching how the session trims the value.
fn resolve_inject_mode(
    captured: &str,
    ambient: Option<String>,
    live: Option<(Option<String>, bool)>,
) -> String {
    let chosen = match live {
        Some((value, true)) => value,
        Some((value, false)) => ambient.or(value),
        None => ambient,
    };
    match chosen {
        Some(value) => {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                captured.to_owned()
            } else {
                trimmed
            }
        }
        None => captured.to_owned(),
    }
}

/// Fold the process environment and the effective live settings into the
/// resolver's inputs. Cheap per press (the per-utterance session reload
/// does the same work) and side-effect free.
fn resolved_live_inject_mode() -> Option<(Option<String>, bool)> {
    crate::config::effective_live_runtime_settings()
        .get("inject_mode")
        .map(|&(_, ref value, configured)| (value.clone(), configured))
}

fn run_with_injector(
    path: &Path,
    mode: &str,
    injector: &mut dyn TextInjector,
    cancellation: Option<Arc<AtomicBool>>,
    target: Option<crate::platform::foreground_window::WindowInfo>,
) -> RuntimeEvent {
    let failure = |error: &str| {
        RuntimeEvent::Stderr(format!(
            "[hotkey] paste-last shortcut failed: {}",
            crate::diag::ascii_escaped(error)
        ))
    };
    // The destination window's profile can override the inject mode: a
    // privacy-sensitive window whose profile says print must not
    // receive a pasted transcript, and an application whose profile
    // requires typing must not get a clipboard paste. Resolution reloads
    // config.json so a
    // Profiles-tab save applies to the next press, mirroring the
    // per-utterance session reload.
    let mut mode = mode.to_owned();
    if let Some(window) = target.as_ref() {
        use crate::dictate::profile::{ProfileMatcher, ReloadingProfileMatcher};
        let applied = ReloadingProfileMatcher::new().resolve(window);
        if let Some(override_mode) = applied.settings.get("inject_mode") {
            mode = override_mode.to_owned();
        }
    }
    // `inject_mode = print` means "transcribe but never inject", so a
    // paste-last shortcut must honour that too — including when the
    // destination window's profile says print. The guard lives here (not
    // only in the native backend) so every injector seam enforces it.
    if mode.trim().eq_ignore_ascii_case("print") {
        return failure(
            "the configured inject_mode is print: transcripts are not injected so there is nothing to paste",
        );
    }
    let (text, row_mode) = match crate::history::last_transcript_with_mode(path) {
        Ok((text, mode)) => (text, mode),
        Err(error) => return failure(&error.to_string()),
    };
    // The utterance payload records the session's EFFECTIVE inject mode,
    // so a per-window profile override is visible per row. A transcript
    // recorded under print mode was never injected; re-pasting it would
    // violate that choice even though the global mode changed since.
    // copy-last is the right action for those transcripts — copying is
    // not injection.
    if row_mode
        .as_deref()
        .is_some_and(|mode| mode.eq_ignore_ascii_case("print"))
    {
        return failure(
            "the most recent transcript was recorded under inject_mode=print for its window, so it was never injected",
        );
    }
    match injector.paste(&text, &mode, cancellation, target) {
        Ok(()) => RuntimeEvent::Stdout("[hotkey] pasted last transcript".to_owned()),
        Err(error) => failure(&error.to_string()),
    }
}

/// Queue one paste job. A held or rapidly repeated key cannot accumulate
/// unbounded injections, and the listener thread never waits for the
/// keystroke burst. `captured_mode` is the install-time inject mode; the
/// worker re-resolves it at press time via [`resolve_inject_mode`].
pub(super) fn queue(
    tx: Sender<RuntimeEvent>,
    busy: Arc<AtomicBool>,
    history_path: PathBuf,
    captured_mode: String,
    cancellation: Option<Arc<AtomicBool>>,
    target: Option<crate::platform::foreground_window::WindowInfo>,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) {
    queue_with_injector(
        tx,
        busy,
        history_path,
        captured_mode,
        cancellation,
        target,
        repaint_notifier,
        NativeInjector,
    );
}

fn queue_with_injector<I: TextInjector + Send + 'static>(
    tx: Sender<RuntimeEvent>,
    busy: Arc<AtomicBool>,
    history_path: PathBuf,
    captured_mode: String,
    cancellation: Option<Arc<AtomicBool>>,
    target: Option<crate::platform::foreground_window::WindowInfo>,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
    mut injector: I,
) {
    if busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let worker_busy = Arc::clone(&busy);
    let failure_tx = tx.clone();
    let failure_notifier = repaint_notifier.clone();
    let spawned = std::thread::Builder::new()
        .name("wd-paste-last".to_owned())
        .spawn(move || {
            struct ResetBusy(Arc<AtomicBool>);
            impl Drop for ResetBusy {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _reset_busy = ResetBusy(worker_busy);
            // Resolve the mode HERE, at press-execution time, so a mode
            // flipped in the settings UI while the runtime stays active
            // applies to the next press .
            let mode = resolve_inject_mode(
                &captured_mode,
                std::env::var(INJECT_MODE_ENV).ok(),
                resolved_live_inject_mode(),
            );
            let _ = tx.send(run_with_injector(
                &history_path,
                &mode,
                &mut injector,
                cancellation,
                target,
            ));
            if let Some(notifier) = repaint_notifier.as_ref() {
                notifier();
            }
        });
    if let Err(error) = spawned {
        busy.store(false, Ordering::Release);
        let _ = failure_tx.send(RuntimeEvent::Stderr(format!(
            "[hotkey] could not start paste-last worker: {}",
            crate::diag::ascii_escaped(&error.to_string())
        )));
        if let Some(notifier) = failure_notifier.as_ref() {
            notifier();
        }
    }
}

#[cfg(test)]
#[path = "paste_last_tests.rs"]
mod tests;
