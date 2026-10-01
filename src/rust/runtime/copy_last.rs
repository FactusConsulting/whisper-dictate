//! Windows GUI copy-last action. The hotkey listener only queues work here;
//! history reads and clipboard subprocesses run outside its message loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc};

use super::RuntimeEvent;
use crate::history::{ClipboardWriter, SubprocessClipboard};

fn run(path: &Path, clipboard: &mut dyn ClipboardWriter) -> RuntimeEvent {
    match crate::history::copy_last_to_clipboard(path, clipboard) {
        Ok(_) => RuntimeEvent::Stdout("[hotkey] copied last transcript".to_owned()),
        Err(error) => RuntimeEvent::Stderr(format!(
            "[hotkey] copy-last shortcut failed: {}",
            crate::diag::ascii_escaped(&error.to_string())
        )),
    }
}

/// Queue one clipboard job. A held or rapidly repeated key cannot accumulate
/// unbounded writes, and the listener thread never waits for `clip.exe`.
pub(super) fn queue(tx: Sender<RuntimeEvent>, busy: Arc<AtomicBool>, history_path: PathBuf) {
    if busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let worker_busy = Arc::clone(&busy);
    let failure_tx = tx.clone();
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
            let _ = tx.send(run(&history_path, &mut SubprocessClipboard));
        });
    if let Err(error) = spawned {
        busy.store(false, Ordering::Release);
        let _ = failure_tx.send(RuntimeEvent::Stderr(format!(
            "[hotkey] could not start copy-last worker: {}",
            crate::diag::ascii_escaped(&error.to_string())
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

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
}
