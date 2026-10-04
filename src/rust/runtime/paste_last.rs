//! Windows GUI paste-last action. The hotkey listener only queues work here;
//! history reads and the injection run outside its message loop.
//!
//! Design mirrors [`super::copy_last`] with one deliberate difference: the
//! paste arm builds a fresh injection backend per invocation on the worker
//! thread instead of reusing a shared handle. `EnigoInjectBackend` is not
//! `Send`, so the shared UI/runtime backend handles cannot cross into this
//! worker; construction is cheap because `Injector::new` performs no system
//! calls and the enigo backend is built lazily on first use. The process-wide
//! injection guard still brackets the keystroke burst through the
//! `inject_guard::global` slot that `install_hotkey` populates.
//!
//! The transcript always comes from local history, which the dictate session
//! records even when its own injection failed (`dictate/session/transcription.rs`
//! records the attempted utterance alongside `inject_error`), so re-pasting a
//! transcript whose original injection failed works without any extra state.
//!
//! A pending dictation paste-restore is never clobbered: the restore timers
//! only rewrite the clipboard when it still holds the text they injected, so
//! an intervening paste-last write simply retires their restore cycle.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc};

use super::RuntimeEvent;

/// Worker seam for tests: production injects through the native backend,
/// tests record the text and mode instead.
trait TextInjector {
    fn paste(&mut self, text: &str, mode: &str) -> anyhow::Result<()>;
}

struct NativeInjector;

impl TextInjector for NativeInjector {
    fn paste(&mut self, text: &str, mode: &str) -> anyhow::Result<()> {
        crate::injection::ui::paste_last_into_focused_window(text, mode)
    }
}

fn run_with_injector(path: &Path, mode: &str, injector: &mut dyn TextInjector) -> RuntimeEvent {
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
    let text = match crate::history::last_transcript(path) {
        Ok(text) => text,
        Err(error) => return failure(&error.to_string()),
    };
    match injector.paste(&text, mode) {
        Ok(()) => RuntimeEvent::Stdout("[hotkey] pasted last transcript".to_owned()),
        Err(error) => failure(&error.to_string()),
    }
}

fn run(path: &Path, mode: &str) -> RuntimeEvent {
    let mut injector = NativeInjector;
    run_with_injector(path, mode, &mut injector)
}

/// Queue one paste job. A held or rapidly repeated key cannot accumulate
/// unbounded injections, and the listener thread never waits for the
/// keystroke burst.
pub(super) fn queue(
    tx: Sender<RuntimeEvent>,
    busy: Arc<AtomicBool>,
    history_path: PathBuf,
    mode: String,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) {
    queue_with_injector(
        tx,
        busy,
        history_path,
        mode,
        repaint_notifier,
        NativeInjector,
    );
}

fn queue_with_injector<I: TextInjector + Send + 'static>(
    tx: Sender<RuntimeEvent>,
    busy: Arc<AtomicBool>,
    history_path: PathBuf,
    mode: String,
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
            let _ = tx.send(run_with_injector(&history_path, &mode, &mut injector));
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
    }

    impl TextInjector for RecordingInjector {
        fn paste(&mut self, text: &str, mode: &str) -> Result<()> {
            self.pasted.push(text.to_owned());
            self.modes.push(mode.to_owned());
            Ok(())
        }
    }

    #[test]
    fn action_pastes_latest_history_without_logging_transcript_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"private transcript\"}\n").unwrap();
        let mut injector = RecordingInjector::default();
        let event = run_with_injector(&path, "auto", &mut injector);
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
        let event = run_with_injector(&path, "auto", &mut injector);
        assert!(injector.pasted.is_empty());
        assert!(matches!(event, RuntimeEvent::Stderr(line) if line.contains("history is empty")));
    }

    #[test]
    fn injection_failure_is_ascii_safe() {
        struct FailingInjector;
        impl TextInjector for FailingInjector {
            fn paste(&mut self, _text: &str, _mode: &str) -> Result<()> {
                anyhow::bail!("Adgang nægtet")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"hello\"}\n").unwrap();
        let event = run_with_injector(&path, "auto", &mut FailingInjector);
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
        let event = run_with_injector(&path, "print", &mut injector);
        assert!(injector.pasted.is_empty());
        assert!(
            matches!(event, RuntimeEvent::Stderr(line) if line.is_ascii() && line.contains("print"))
        );
        // The native backend rejects it too; the worker guard covers every
        // injector seam regardless.
        let event = run_with_injector(&path, "PRINT", &mut injector);
        assert!(injector.pasted.is_empty());
        assert!(matches!(event, RuntimeEvent::Stderr(_)));
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
        let event = run(&std::path::Path::new("").join("missing.jsonl"), "auto");
        let RuntimeEvent::Stderr(line) = event else {
            panic!("expected a stderr event");
        };
        assert!(line.is_ascii());
        assert!(line.starts_with("[hotkey] paste-last shortcut failed: "));
        assert!(line.contains("history is empty"));
    }
}
