//! Windows GUI copy-last action. The hotkey listener only queues work here;
//! history reads and clipboard subprocesses run outside its message loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc};

use super::RuntimeEvent;
use crate::history::ClipboardWriter;

/// Use the native Unicode clipboard path rather than piping UTF-8 into
/// clip.exe, which decodes stdin through the active console code page.
struct NativeClipboard;

impl ClipboardWriter for NativeClipboard {
    fn copy(&mut self, text: &str) -> anyhow::Result<()> {
        arboard::Clipboard::new()?.set_text(text.to_owned())?;
        Ok(())
    }
}

struct CancelBeforeCopy<'a, F> {
    clipboard: &'a mut dyn ClipboardWriter,
    cancel: Option<F>,
}

impl<F: FnOnce()> ClipboardWriter for CancelBeforeCopy<'_, F> {
    fn copy(&mut self, text: &str) -> anyhow::Result<()> {
        if let Some(cancel) = self.cancel.take() {
            cancel();
        }
        self.clipboard.copy(text)
    }
}

fn run_with_cancel(
    path: &Path,
    clipboard: &mut dyn ClipboardWriter,
    cancel: impl FnOnce(),
) -> RuntimeEvent {
    let mut clipboard = CancelBeforeCopy {
        clipboard,
        cancel: Some(cancel),
    };
    match crate::history::copy_last_to_clipboard(path, &mut clipboard) {
        Ok(_) => RuntimeEvent::Stdout("[hotkey] copied last transcript".to_owned()),
        Err(error) => RuntimeEvent::Stderr(format!(
            "[hotkey] copy-last shortcut failed: {}",
            crate::diag::ascii_escaped(&error.to_string())
        )),
    }
}

fn run(path: &Path, clipboard: &mut dyn ClipboardWriter) -> RuntimeEvent {
    run_with_cancel(
        path,
        clipboard,
        crate::injection::cancel_ui_clipboard_restore,
    )
}

/// Queue one clipboard job. A held or rapidly repeated key cannot accumulate
/// unbounded writes, and the listener thread never waits for `clip.exe`.
pub(super) fn queue(
    tx: Sender<RuntimeEvent>,
    busy: Arc<AtomicBool>,
    history_path: PathBuf,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) {
    queue_with_writer(tx, busy, history_path, repaint_notifier, NativeClipboard);
}

fn queue_with_writer<W: ClipboardWriter + Send + 'static>(
    tx: Sender<RuntimeEvent>,
    busy: Arc<AtomicBool>,
    history_path: PathBuf,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
    mut clipboard: W,
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
        .name("wd-copy-last".to_owned())
        .spawn(move || {
            struct ResetBusy(Arc<AtomicBool>);
            impl Drop for ResetBusy {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _reset_busy = ResetBusy(worker_busy);
            let _ = tx.send(run(&history_path, &mut clipboard));
            if let Some(notifier) = repaint_notifier.as_ref() {
                notifier();
            }
        });
    if let Err(error) = spawned {
        busy.store(false, Ordering::Release);
        let _ = failure_tx.send(RuntimeEvent::Stderr(format!(
            "[hotkey] could not start copy-last worker: {}",
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
    use std::cell::Cell;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct RecordingClipboard(Vec<String>);

    impl ClipboardWriter for RecordingClipboard {
        fn copy(&mut self, text: &str) -> Result<()> {
            self.0.push(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn action_copies_latest_history_without_logging_transcript_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"private transcript\"}\n").unwrap();
        let mut clipboard = RecordingClipboard::default();
        let event = run(&path, &mut clipboard);
        assert_eq!(clipboard.0, ["private transcript"]);
        assert!(
            matches!(event, RuntimeEvent::Stdout(line) if line == "[hotkey] copied last transcript")
        );
    }

    #[test]
    fn action_reports_empty_history_without_touching_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");
        let mut clipboard = RecordingClipboard::default();
        let event = run(&path, &mut clipboard);
        assert!(clipboard.0.is_empty());
        assert!(matches!(event, RuntimeEvent::Stderr(line) if line.contains("history is empty")));
    }

    #[test]
    fn action_cancels_pending_restore_immediately_before_copy() {
        struct AssertCancelled<'a>(&'a Cell<bool>);
        impl ClipboardWriter for AssertCancelled<'_> {
            fn copy(&mut self, text: &str) -> Result<()> {
                assert!(self.0.get(), "pending restore must be cancelled first");
                assert_eq!(text, "rødgrød æøå");
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"rødgrød æøå\"}\n").unwrap();
        let cancelled = Cell::new(false);
        let event = run_with_cancel(&path, &mut AssertCancelled(&cancelled), || {
            cancelled.set(true)
        });
        assert!(matches!(event, RuntimeEvent::Stdout(_)));
        assert!(cancelled.get());

        cancelled.set(false);
        let event = run_with_cancel(
            &dir.path().join("missing.jsonl"),
            &mut AssertCancelled(&cancelled),
            || cancelled.set(true),
        );
        assert!(matches!(event, RuntimeEvent::Stderr(_)));
        assert!(
            !cancelled.get(),
            "an empty history must not claim the clipboard"
        );
    }

    #[test]
    fn localized_clipboard_error_is_ascii_safe() {
        struct FailingClipboard;
        impl ClipboardWriter for FailingClipboard {
            fn copy(&mut self, _text: &str) -> Result<()> {
                anyhow::bail!("Adgang nægtet")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"hello\"}\n").unwrap();
        let event = run(&path, &mut FailingClipboard);
        assert!(
            matches!(event, RuntimeEvent::Stderr(line) if line.is_ascii() && line.contains("\\u{e6}"))
        );
    }

    #[test]
    fn queued_copy_wakes_ui_after_reporting_success_or_failure() {
        fn wait_until(predicate: impl Fn() -> bool) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !predicate() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(
                predicate(),
                "copy worker did not finish before the deadline"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"copy me\"}\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let repaints = Arc::new(AtomicUsize::new(0));
        let counts = Arc::clone(&repaints);
        let notifier: crate::runtime::supervisor::RepaintNotifier = Arc::new(move || {
            counts.fetch_add(1, Ordering::SeqCst);
        });
        queue_with_writer(
            tx.clone(),
            Arc::clone(&busy),
            path,
            Some(notifier.clone()),
            RecordingClipboard::default(),
        );
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(RuntimeEvent::Stdout(_))
        ));
        wait_until(|| repaints.load(Ordering::SeqCst) == 1 && !busy.load(Ordering::Acquire));

        queue_with_writer(
            tx,
            Arc::clone(&busy),
            dir.path().join("missing.jsonl"),
            Some(notifier),
            RecordingClipboard::default(),
        );
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(RuntimeEvent::Stderr(_))
        ));
        wait_until(|| repaints.load(Ordering::SeqCst) == 2 && !busy.load(Ordering::Acquire));
        assert_eq!(repaints.load(Ordering::SeqCst), 2);
    }

    // Manual-only: the Windows clipboard is process-global. An automated test
    // must not replace rich formats or race with the user's own copy action.
    #[test]
    #[ignore = "writes the real system clipboard; run deliberately on a disposable session"]
    fn native_clipboard_preserves_unicode_when_text_clipboard_is_available() {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return;
        };
        let Ok(previous) = clipboard.get_text() else {
            return;
        };
        if crate::injection::ui::clipboard_has_only_text_formats() != Some(true) {
            return;
        }
        struct Restore(arboard::Clipboard, String);
        impl Drop for Restore {
            fn drop(&mut self) {
                // Never overwrite an intervening user copy.
                if self
                    .0
                    .get_text()
                    .is_ok_and(|current| current == "rødgrød æøå")
                {
                    let _ = self.0.set_text(self.1.clone());
                }
            }
        }
        let mut restore = Restore(clipboard, previous);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "{\"text\":\"rødgrød æøå\"}\n").unwrap();
        assert!(matches!(
            run(&path, &mut NativeClipboard),
            RuntimeEvent::Stdout(_)
        ));
        assert_eq!(restore.0.get_text().unwrap(), "rødgrød æøå");
    }
}
