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
//! the runtime stopped.
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
    ) -> anyhow::Result<()>;
}

struct NativeInjector;

impl TextInjector for NativeInjector {
    fn paste(
        &mut self,
        text: &str,
        mode: &str,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<()> {
        crate::injection::ui::paste_last_into_focused_window(text, mode, cancellation)
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
) -> RuntimeEvent {
    let failure = |error: &str| {
        RuntimeEvent::Stderr(format!(
            "[hotkey] paste-last shortcut failed: {}",
            crate::diag::ascii_escaped(error)
        ))
    };
    // `inject_mode = print` means "transcribe but never inject", so a
    // paste-last shortcut must honour that too. The guard lives here (not
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
    match injector.paste(&text, mode, cancellation) {
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
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) {
    queue_with_injector(
        tx,
        busy,
        history_path,
        captured_mode,
        cancellation,
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
            // applies to the next press (Codex P2 in_process.rs:397).
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
mod tests {
    use super::*;
    use anyhow::Result;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct RecordingInjector {
        pasted: Vec<String>,
        modes: Vec<String>,
        cancellations: Vec<Option<Arc<AtomicBool>>>,
    }

    impl TextInjector for RecordingInjector {
        fn paste(
            &mut self,
            text: &str,
            mode: &str,
            cancellation: Option<Arc<AtomicBool>>,
        ) -> Result<()> {
            self.pasted.push(text.to_owned());
            self.modes.push(mode.to_owned());
            self.cancellations.push(cancellation);
            Ok(())
        }
    }

    #[test]
    fn action_pastes_latest_history_without_logging_transcript_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"private transcript\"}\n").unwrap();
        let mut injector = RecordingInjector::default();
        let event = run_with_injector(&path, "auto", &mut injector, None);
        assert_eq!(injector.pasted, ["private transcript"]);
        assert_eq!(injector.modes, ["auto"]);
        assert!(
            matches!(event, RuntimeEvent::Stdout(line) if line == "[hotkey] pasted last transcript")
        );
    }

    #[test]
    fn action_reports_empty_history_without_injecting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");
        let mut injector = RecordingInjector::default();
        let event = run_with_injector(&path, "auto", &mut injector, None);
        assert!(injector.pasted.is_empty());
        assert!(matches!(event, RuntimeEvent::Stderr(line) if line.contains("history is empty")));
    }

    #[test]
    fn injection_failure_is_ascii_safe() {
        struct FailingInjector;
        impl TextInjector for FailingInjector {
            fn paste(
                &mut self,
                _text: &str,
                _mode: &str,
                _cancel: Option<Arc<AtomicBool>>,
            ) -> Result<()> {
                anyhow::bail!("Adgang nægtet")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"hello\"}\n").unwrap();
        let event = run_with_injector(&path, "auto", &mut FailingInjector, None);
        assert!(
            matches!(event, RuntimeEvent::Stderr(line) if line.is_ascii() && line.contains("\\u{e6}"))
        );
    }

    #[test]
    fn print_mode_is_rejected_without_injecting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"hello\"}\n").unwrap();
        let mut injector = RecordingInjector::default();
        let event = run_with_injector(&path, "print", &mut injector, None);
        assert!(injector.pasted.is_empty());
        assert!(
            matches!(event, RuntimeEvent::Stderr(line) if line.is_ascii() && line.contains("print"))
        );
        // The native backend rejects it too; the worker guard covers every
        // injector seam regardless.
        let event = run_with_injector(&path, "PRINT", &mut injector, None);
        assert!(injector.pasted.is_empty());
        assert!(matches!(event, RuntimeEvent::Stderr(_)));
    }

    #[test]
    fn row_recorded_under_print_mode_is_rejected_without_injecting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        // The utterance payload records the session's EFFECTIVE mode, so a
        // per-window profile override shows up per row even when the global
        // mode is auto (Codex P2 in_process.rs:398).
        std::fs::write(&path, "{\"text\":\"private\",\"inject_mode\":\"print\"}\n").unwrap();
        let mut injector = RecordingInjector::default();
        let event = run_with_injector(&path, "auto", &mut injector, None);
        assert!(injector.pasted.is_empty());
        assert!(
            matches!(event, RuntimeEvent::Stderr(line) if line.is_ascii() && line.contains("print"))
        );
        // A newer row recorded under a different mode pastes fine.
        std::fs::write(&path, "{\"text\":\"typed\",\"inject_mode\":\"type\"}\n").unwrap();
        let mut injector = RecordingInjector::default();
        let event = run_with_injector(&path, "auto", &mut injector, None);
        assert_eq!(injector.pasted, ["typed"]);
        assert!(matches!(event, RuntimeEvent::Stdout(_)));
        // Rows written before the field existed paste fine too.
        std::fs::write(&path, "{\"text\":\"legacy\"}\n").unwrap();
        let mut injector = RecordingInjector::default();
        let event = run_with_injector(&path, "auto", &mut injector, None);
        assert_eq!(injector.pasted, ["legacy"]);
        assert!(matches!(event, RuntimeEvent::Stdout(_)));
    }

    #[test]
    fn queued_paste_wakes_ui_after_reporting_success_or_failure() {
        fn wait_until(predicate: impl Fn() -> bool) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !predicate() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(
                predicate(),
                "paste worker did not finish before the deadline"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"paste me\"}\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let repaints = Arc::new(AtomicUsize::new(0));
        let counts = Arc::clone(&repaints);
        let notifier: crate::runtime::supervisor::RepaintNotifier = Arc::new(move || {
            counts.fetch_add(1, Ordering::SeqCst);
        });
        queue_with_injector(
            tx.clone(),
            Arc::clone(&busy),
            path,
            "auto".to_owned(),
            None,
            Some(notifier.clone()),
            RecordingInjector::default(),
        );
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(RuntimeEvent::Stdout(_))
        ));
        wait_until(|| repaints.load(Ordering::SeqCst) == 1 && !busy.load(Ordering::Acquire));

        queue_with_injector(
            tx,
            Arc::clone(&busy),
            dir.path().join("missing.jsonl"),
            "auto".to_owned(),
            None,
            Some(notifier),
            RecordingInjector::default(),
        );
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(RuntimeEvent::Stderr(_))
        ));
        wait_until(|| repaints.load(Ordering::SeqCst) == 2 && !busy.load(Ordering::Acquire));
        assert_eq!(repaints.load(Ordering::SeqCst), 2);
    }

    // Manual-only: a real injection requires a focused window plus the
    // rust-injection backend. Cover the decision points instead: the
    // empty-history guard above, the print-mode guard, and the ASCII
    // error contract. The end-to-end paste is exercised by the manual
    // test checklist with a configured paste_last_hotkey.
    #[test]
    fn worker_reports_missing_history_as_ascii() {
        let mut injector = RecordingInjector::default();
        let event = run_with_injector(
            &std::path::Path::new("").join("missing.jsonl"),
            "auto",
            &mut injector,
            None,
        );
        let RuntimeEvent::Stderr(line) = event else {
            panic!("expected a stderr event");
        };
        assert!(line.is_ascii());
        assert!(line.starts_with("[hotkey] paste-last shortcut failed: "));
        assert!(line.contains("history is empty"));
    }

    #[test]
    fn inject_mode_resolution_mirrors_the_live_reload_precedence() {
        // A configured config.json value outranks everything (the ambient
        // environment and the install-time snapshot).
        assert_eq!(
            resolve_inject_mode(
                "type",
                Some("print".to_owned()),
                Some((Some("paste".to_owned()), true)),
            ),
            "paste"
        );
        // Not configured: the ambient environment outranks the schema default.
        assert_eq!(
            resolve_inject_mode(
                "type",
                Some("print".to_owned()),
                Some((Some("auto".to_owned()), false)),
            ),
            "print"
        );
        // No ambient value: the schema default applies.
        assert_eq!(
            resolve_inject_mode("type", None, Some((Some("auto".to_owned()), false))),
            "auto"
        );
        // Settings lookup failure: the install-time snapshot survives.
        assert_eq!(resolve_inject_mode("paste", None, None), "paste");
        // A blank configured value falls through to the snapshot.
        assert_eq!(
            resolve_inject_mode("type", None, Some((Some("  ".to_owned()), true))),
            "type"
        );
    }

    #[test]
    fn queued_paste_forwards_the_runtime_lifecycle_flag_to_the_injector() {
        struct FlagInjector {
            saw_flag: Arc<AtomicBool>,
        }
        impl TextInjector for FlagInjector {
            fn paste(
                &mut self,
                _text: &str,
                _mode: &str,
                cancellation: Option<Arc<AtomicBool>>,
            ) -> Result<()> {
                if cancellation.is_some() {
                    self.saw_flag.store(true, Ordering::Release);
                }
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"flag\"}\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let saw_flag = Arc::new(AtomicBool::new(false));
        queue_with_injector(
            tx,
            Arc::clone(&busy),
            path,
            "auto".to_owned(),
            Some(Arc::new(AtomicBool::new(true))),
            None,
            FlagInjector {
                saw_flag: Arc::clone(&saw_flag),
            },
        );
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(RuntimeEvent::Stdout(_))
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !saw_flag.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            saw_flag.load(Ordering::Acquire),
            "flag never reached the injector"
        );
        assert!(!busy.load(Ordering::Acquire));
    }
}
