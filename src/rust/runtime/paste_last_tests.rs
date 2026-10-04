//! Companion tests for [`crate::runtime::paste_last`].
//!
//! Extracted from an inline `#[cfg(test)] mod tests` so the production
//! module stays under the repository's ~500-line limit (AGENTS.md
//! modularity rule) and the regression-test discipline scanner resolves
//! the matching `paste_last_tests.rs` sibling. Wired from
//! `runtime/mod.rs`-adjacent `paste_last.rs` via
//! `#[cfg(test)] #[path = "paste_last_tests.rs"] mod tests;`, so this
//! file is a SIBLING of `paste_last` and reaches the module with
//! `use super::*` (private-but-test-visible items are visible because
//! the sibling module is declared inside `paste_last.rs`'s namespace).

use super::*;
use anyhow::Result;
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct RecordingInjector {
    pasted: Vec<String>,
    modes: Vec<String>,
    cancellations: Vec<Option<Arc<AtomicBool>>>,
    targets: Vec<Option<crate::platform::foreground_window::WindowInfo>>,
}

impl TextInjector for RecordingInjector {
    fn paste(
        &mut self,
        text: &str,
        mode: &str,
        cancellation: Option<Arc<AtomicBool>>,
        target: Option<crate::platform::foreground_window::WindowInfo>,
    ) -> Result<()> {
        self.pasted.push(text.to_owned());
        self.modes.push(mode.to_owned());
        self.cancellations.push(cancellation);
        self.targets.push(target);
        Ok(())
    }
}

#[test]
fn action_pastes_latest_history_without_logging_transcript_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    std::fs::write(&path, "{\"text\":\"private transcript\"}\n").unwrap();
    let mut injector = RecordingInjector::default();
    let event = run_with_injector(&path, "auto", &mut injector, None, None);
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
    let event = run_with_injector(&path, "auto", &mut injector, None, None);
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
            _target: Option<crate::platform::foreground_window::WindowInfo>,
        ) -> Result<()> {
            anyhow::bail!("Adgang nægtet")
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    std::fs::write(&path, "{\"text\":\"hello\"}\n").unwrap();
    let event = run_with_injector(&path, "auto", &mut FailingInjector, None, None);
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
    let event = run_with_injector(&path, "print", &mut injector, None, None);
    assert!(injector.pasted.is_empty());
    assert!(
        matches!(event, RuntimeEvent::Stderr(line) if line.is_ascii() && line.contains("print"))
    );
    // The native backend rejects it too; the worker guard covers every
    // injector seam regardless.
    let event = run_with_injector(&path, "PRINT", &mut injector, None, None);
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
    let event = run_with_injector(&path, "auto", &mut injector, None, None);
    assert!(injector.pasted.is_empty());
    assert!(
        matches!(event, RuntimeEvent::Stderr(line) if line.is_ascii() && line.contains("print"))
    );
    // A newer row recorded under a different mode pastes fine.
    std::fs::write(&path, "{\"text\":\"typed\",\"inject_mode\":\"type\"}\n").unwrap();
    let mut injector = RecordingInjector::default();
    let event = run_with_injector(&path, "auto", &mut injector, None, None);
    assert_eq!(injector.pasted, ["typed"]);
    assert!(matches!(event, RuntimeEvent::Stdout(_)));
    // Rows written before the field existed paste fine too.
    std::fs::write(&path, "{\"text\":\"legacy\"}\n").unwrap();
    let mut injector = RecordingInjector::default();
    let event = run_with_injector(&path, "auto", &mut injector, None, None);
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
            _target: Option<crate::platform::foreground_window::WindowInfo>,
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
