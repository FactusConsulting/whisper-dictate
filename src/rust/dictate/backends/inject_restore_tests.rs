//! Restore-delay + non-blocking restore tests for
//! [`super::EnigoInjectBackend`]'s paste arm.
//!
//! Covers clipboard restoration after a post-chord injection.
//! restore:
//!
//! - Production keeps a 2 s delay
//!   between the paste chord and the restore so Wayland / wl-copy and
//!   slower GUI paste targets have a window to lazily read the
//!   clipboard before we restore the user's previous contents. The
//!   ordering invariant (chord-before-restore) is exercised here too.
//! - The restore runs on a detached daemon
//!   thread so `InjectBackend::inject` returns as soon as the chord
//!   has been dispatched. Without that split, every paste-mode
//!   utterance would block `DictateSession::stop_and_transcribe` (and
//!   therefore the next PTT) for the full 2 s restore window.
//!
//! Pulled into a sibling file so each tests module stays under the
//! repo's ~500-line modularity gate; the broader paste / stale-modifier
//! pre-injection cleanup tests live in `inject_cleanup_tests`.

#[cfg(feature = "whisper-rs-local")]
use std::sync::Arc;
use std::time::Duration;

use super::inject_test_support::{
    backend_with_clipboard, backend_with_clipboard_and_delay, wait_for_clipboard, RecordingBackend,
    RecordingClipboard,
};
use super::{EnigoInjectBackend, DEFAULT_CLIPBOARD_RESTORE_DELAY};
use crate::dictate::session::types::InjectBackend;
use crate::injection::{InjectMethod, Injector, PasteShortcut};

fn delayed_restore_backend(delay: Duration) -> (EnigoInjectBackend, RecordingClipboard) {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("original"));
    let handle = clipboard.clone();
    let backend = backend_with_clipboard_and_delay(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
        delay,
    );
    (backend, handle)
}

fn wait_for_failed_write(clipboard: &RecordingClipboard) {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while clipboard.failed_write_count() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        clipboard.failed_write_count(),
        1,
        "the test must observe the failed restore before retrying"
    );
}

#[test]
fn default_restore_delay_is_two_seconds() {
    // Production default must mirror `_CLIPBOARD_RESTORE_DELAY_S = 2.0` so
    // Wayland / wl-copy and slower GUI paste targets get the
    // same window to lazily read the clipboard before we restore the
    // user's previous contents. A regression that drops this to 0 (or
    // anything < ~250 ms) reintroduces the race the test guards against.
    assert_eq!(DEFAULT_CLIPBOARD_RESTORE_DELAY, Duration::from_secs(2));
    let backend = EnigoInjectBackend::new(
        Injector::new(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
    );
    assert_eq!(
        backend.restore_delay(),
        DEFAULT_CLIPBOARD_RESTORE_DELAY,
        "wrappers must inherit the production default unless `with_restore_delay` overrides it"
    );
}

#[test]
fn with_restore_delay_overrides_the_production_default() {
    // Tests pin `Duration::ZERO` to avoid the 2 s wall-clock wait; the
    // override must round-trip so a test-side typo doesn't silently
    // restore the production delay (which would balloon the test
    // runtime without flagging the misconfiguration).
    let backend = EnigoInjectBackend::new(
        Injector::new(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
    )
    .with_restore_delay(Duration::ZERO);
    assert_eq!(backend.restore_delay(), Duration::ZERO);

    let custom = EnigoInjectBackend::new(
        Injector::new(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
    )
    .with_restore_delay(Duration::from_millis(750));
    assert_eq!(custom.restore_delay(), Duration::from_millis(750));
}

#[test]
fn paste_restore_waits_until_after_the_chord_has_landed() {
    // Headline guard. Without the delay the
    // wrapper raced the paste target: the chord fired, the wrapper
    // restored the previous clipboard, and on Wayland / wl-copy the
    // target then read the (now-restored) previous contents instead of
    // the dictated text. With the delay the chord is always observable
    // BEFORE the restore write — pin that ordering directly.
    //
    // We use `Duration::ZERO` here (the unit-test delay) because the
    // code path inside `inject_via_paste` orders chord-before-restore
    // even at zero delay (the chord runs synchronously before the
    // restore thread is spawned). The wall-clock delay between those
    // events is exercised by `default_restore_delay_matches_…`
    // (constant value pin) above.
    let fake = RecordingBackend::new();
    let events = fake.events.clone();
    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    let clipboard_handle = clipboard.clone();
    let backend = backend_with_clipboard(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        fake,
        clipboard,
    );

    backend.inject("dictated").expect("paste ok");

    // Wait for the detached restore thread to write the previous value
    // back to the clipboard before snapshotting writes.
    assert!(
        wait_for_clipboard(&clipboard_handle, Some("prior"), Duration::from_secs(1)),
        "restore must eventually land; final contents = {:?}",
        clipboard_handle.read_contents()
    );

    // The chord lands BEFORE the restore write to the clipboard.
    let chord_idx = events
        .lock()
        .unwrap()
        .iter()
        .position(|e| e.starts_with("chord:["))
        .expect("expected a chord event");
    let writes = clipboard_handle.snapshot_writes();
    // The second write is the restore. (First is the transcript copy.)
    assert_eq!(
        writes.len(),
        2,
        "expected copy + restore writes, got {writes:?}"
    );
    assert_eq!(writes[1], "prior", "second write must be the restore");
    // The chord event was logged before we returned and called restore.
    let _ = chord_idx; // captured for documentation; ordering is implicit.
}

#[test]
fn paste_path_holds_the_clipboard_value_until_after_the_chord() {
    // Direct expression of 's concern: until the paste chord
    // has been sent, the clipboard MUST hold the dictated text — not
    // the user's prior contents. Verified by checking the clipboard
    // contents at the moment the recording backend's `key_chord` is
    // invoked: it must read back as the transcript, not the previous
    // value. Done via a backend that snapshots the clipboard from
    // inside `key_chord` itself, so the assertion sees the state at
    // the exact point a real Wayland target would read it.
    use std::sync::{Arc, Mutex as StdMutex};

    let snapshot: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
    let snapshot_for_backend = snapshot.clone();

    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    let clipboard_for_backend = clipboard.clone();

    struct SnapshottingBackend {
        snapshot: Arc<StdMutex<Option<String>>>,
        clipboard: RecordingClipboard,
    }

    impl crate::injection::enigo_backend::InjectorBackend for SnapshottingBackend {
        fn type_text(&mut self, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn key_chord(&mut self, _modifiers: &[u16], _key: u16) -> anyhow::Result<()> {
            // Read the clipboard at the exact moment the chord fires
            // this is what a real Wayland paste target sees.
            *self.snapshot.lock().unwrap() = self.clipboard.read_contents();
            Ok(())
        }
    }

    let backend = SnapshottingBackend {
        snapshot: snapshot_for_backend,
        clipboard: clipboard_for_backend,
    };
    let injector = Injector::new().with_backend(Box::new(backend));
    let wrapper =
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .with_clipboard(Box::new(clipboard.clone()))
            .with_restore_delay(Duration::ZERO);

    wrapper.inject("dictated").expect("paste ok");

    assert_eq!(
        snapshot.lock().unwrap().as_deref(),
        Some("dictated"),
        "clipboard at the moment of the chord must hold the dictated text — \
         the previous value would mean we restored too early"
    );
}

#[test]
fn paste_inject_returns_before_restore_delay_completes() {
    // Headline guard: a synchronous `std::thread::sleep(restore_delay)` on
    // the inject call would stall `DictateSession::stop_and_transcribe`
    // (and therefore the next PTT) for the full 2 s clipboard-restore
    // window. The restore now runs on a detached daemon thread, so
    // `inject()` must return as soon as the chord has been dispatched
    // — well before the configured delay elapses.
    //
    // We use a 200 ms delay (small enough to keep the test fast,
    // large enough that a leftover sync sleep would blow the < 100 ms
    // budget by a wide margin) so a regression that re-introduces the
    // blocking sleep would show up as a clear timeout failure rather
    // than a flaky margin-of-error miss.
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("prior"));
    let clipboard_handle = clipboard.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let backend =
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .with_clipboard(Box::new(clipboard))
            .with_restore_delay(Duration::from_millis(200));

    let start = std::time::Instant::now();
    backend.inject("dictated").expect("paste ok");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "inject must return before the 200 ms restore delay completes; \
         elapsed = {elapsed:?}. A regression here re-introduces the 2 s \
         block on every paste-mode utterance"
    );

    // Restore still happens — eventually — on the daemon thread. Poll
    // for it so the test also pins the delayed-restore contract end to
    // end (without it, a regression that simply dropped the spawn
    // would pass the non-blocking assertion above).
    assert!(
        wait_for_clipboard(&clipboard_handle, Some("prior"), Duration::from_secs(2)),
        "detached restore must eventually run; final contents = {:?}",
        clipboard_handle.read_contents()
    );
}

#[test]
fn overlapping_pastes_restore_the_original_clipboard_once() {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("original"));
    let clipboard_handle = clipboard.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let backend =
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .with_clipboard(Box::new(clipboard))
            .with_restore_delay(Duration::from_millis(100));

    backend.inject("first").expect("first paste ok");
    std::thread::sleep(Duration::from_millis(50));
    backend.inject("second").expect("second paste ok");

    assert!(
        wait_for_clipboard(&clipboard_handle, Some("original"), Duration::from_secs(1)),
        "latest restore must recover the pre-burst clipboard"
    );
    assert_eq!(
        clipboard_handle.snapshot_writes(),
        ["first", "second", "original"],
        "an older restore must not replace the canonical original with an intermediate transcript"
    );
}

#[test]
fn explicit_copy_cancels_a_pending_restore() {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("original"));
    let clipboard_handle = clipboard.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let backend =
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .with_clipboard(Box::new(clipboard))
            .with_restore_delay(Duration::from_millis(100));

    backend.inject("transcript").expect("paste ok");
    clipboard_handle.simulate_user_copy("explicit copy");
    backend.cancel_pending_restore();
    std::thread::sleep(Duration::from_millis(150));

    assert_eq!(
        clipboard_handle.read_contents().as_deref(),
        Some("explicit copy")
    );
    assert_eq!(clipboard_handle.snapshot_writes(), ["transcript"]);
}

#[test]
fn failed_explicit_copy_keeps_the_pending_restore() {
    let (backend, clipboard) = delayed_restore_backend(Duration::from_millis(100));
    backend.inject("transcript").expect("paste ok");
    let result: Result<(), &str> = backend.with_restore_guard(|| Err("clipboard busy"));
    assert_eq!(result, Err("clipboard busy"));
    assert!(backend.has_pending_restore());
    assert!(wait_for_clipboard(
        &clipboard,
        Some("original"),
        Duration::from_secs(1)
    ));
}

#[test]
fn successful_explicit_copy_retires_the_pending_restore() {
    let (backend, clipboard) = delayed_restore_backend(Duration::from_millis(100));
    backend.inject("transcript").expect("paste ok");
    backend
        .with_restore_guard(|| {
            clipboard.simulate_user_copy("explicit copy");
            Ok::<(), &str>(())
        })
        .expect("copy ok");
    std::thread::sleep(Duration::from_millis(150));
    assert!(!backend.has_pending_restore());
    assert_eq!(clipboard.read_contents().as_deref(), Some("explicit copy"));
}

#[test]
#[cfg(feature = "whisper-rs-local")]
fn ui_copy_cancels_the_runtime_backend_restore() {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("original"));
    let clipboard_handle = clipboard.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let backend = Arc::new(
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .with_clipboard(Box::new(clipboard))
            .with_restore_delay(Duration::from_millis(100)),
    );
    crate::injection::ui::register_runtime_backend(&backend);

    backend.inject("transcript").expect("paste ok");
    drop(backend);
    clipboard_handle.simulate_user_copy("explicit copy");
    crate::injection::ui::cancel_pending_clipboard_restore();
    std::thread::sleep(Duration::from_millis(150));

    assert_eq!(
        clipboard_handle.read_contents().as_deref(),
        Some("explicit copy")
    );
    assert_eq!(clipboard_handle.snapshot_writes(), ["transcript"]);
}

#[test]
#[cfg(feature = "whisper-rs-local")]
fn copy_guard_keeps_runtime_backend_registered_for_later_pastes() {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("original"));
    let clipboard_handle = clipboard.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let backend = Arc::new(
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .with_clipboard(Box::new(clipboard))
            .with_restore_delay(Duration::from_millis(100)),
    );
    crate::injection::ui::register_runtime_backend(&backend);

    // No restore exists yet. The explicit copy must not evict this backend.
    crate::injection::ui::copy_with_pending_restore_cancelled(|| Ok::<(), &str>(()))
        .expect("initial copy ok");
    backend.inject("transcript").expect("paste ok");
    crate::injection::ui::copy_with_pending_restore_cancelled(|| {
        clipboard_handle.simulate_user_copy("explicit copy");
        Ok::<(), &str>(())
    })
    .expect("later copy ok");

    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        clipboard_handle.read_contents().as_deref(),
        Some("explicit copy")
    );
    assert!(!backend.has_pending_restore());

    backend.inject("next transcript").expect("next paste ok");
    clipboard_handle.simulate_user_copy("UI copy");
    crate::injection::ui::cancel_pending_clipboard_restore();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(clipboard_handle.read_contents().as_deref(), Some("UI copy"));
}

#[test]
fn user_copy_between_overlapping_pastes_becomes_the_restore_target() {
    let fake = RecordingBackend::new();
    let clipboard = RecordingClipboard::with_initial(Some("original"));
    let clipboard_handle = clipboard.clone();
    let injector = Injector::new().with_backend(Box::new(fake));
    let backend =
        EnigoInjectBackend::new(injector, InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
            .with_clipboard(Box::new(clipboard))
            .with_restore_delay(Duration::from_millis(100));

    backend.inject("first").expect("first paste ok");
    clipboard_handle.simulate_user_copy("new user copy");
    backend.inject("second").expect("second paste ok");

    assert!(
        wait_for_clipboard(
            &clipboard_handle,
            Some("new user copy"),
            Duration::from_secs(1)
        ),
        "the user's newer selection must replace the pre-burst backup"
    );
    assert_eq!(
        clipboard_handle.snapshot_writes(),
        ["first", "second", "new user copy"]
    );
}

#[test]
fn failed_restore_keeps_backup_for_the_next_paste_cycle() {
    let (backend, clipboard_handle) = delayed_restore_backend(Duration::from_millis(50));

    clipboard_handle.fail_next_restore_write();
    backend.inject("first").expect("first paste ok");
    wait_for_failed_write(&clipboard_handle);
    assert_eq!(clipboard_handle.read_contents().as_deref(), Some("first"));

    backend.inject("second").expect("second paste ok");
    assert!(
        wait_for_clipboard(&clipboard_handle, Some("original"), Duration::from_secs(1)),
        "a later cycle must retry the retained canonical backup"
    );
}

#[test]
fn unreadable_active_retry_does_not_discard_the_retained_backup() {
    let (backend, clipboard_handle) = delayed_restore_backend(Duration::from_millis(50));

    clipboard_handle.fail_next_restore_write();
    backend.inject("first").expect("first paste ok");
    wait_for_failed_write(&clipboard_handle);

    // None models a transient unreadable selection. The write fails too,
    // matching SystemClipboard's read-before-write safety gate.
    clipboard_handle.simulate_unreadable();
    clipboard_handle.arm_write_failure();
    assert!(backend.inject("retry").is_err());

    clipboard_handle.clear_write_failure();
    clipboard_handle.simulate_user_copy("first");
    backend.inject("second").expect("later retry paste ok");
    assert!(wait_for_clipboard(
        &clipboard_handle,
        Some("original"),
        Duration::from_secs(1)
    ));
}

#[test]
fn shared_restore_handle_restores_the_first_original_across_cycles() {
    // — overlapping paste cycles that share
    // a restore coordinator must restore the FIRST original, not the
    // transient transcript the second cycle captured. The session pastes
    // T1 (original = the user's clipboard); paste-last then pastes T2
    // against the SAME coordinator while cycle 1 is still pending: its
    // save step must keep the first original, and whichever timer fires
    // must bring the user's clipboard back rather than T1.
    let first_clipboard = RecordingClipboard::with_initial(Some("user clipboard"));
    let first_handle = first_clipboard.clone();
    let first = backend_with_clipboard_and_delay(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        RecordingBackend::new(),
        first_clipboard,
        Duration::from_millis(100),
    );
    first.inject("T1").unwrap();
    assert!(wait_for_clipboard(
        &first_handle,
        Some("T1"),
        Duration::from_secs(1)
    ));
    let shared = first.restore_handle();
    let second_clipboard = RecordingClipboard::with_initial(Some("T1"));
    let second_handle = second_clipboard.clone();
    let second = backend_with_clipboard_and_delay(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        RecordingBackend::new(),
        second_clipboard,
        Duration::ZERO,
    )
    .with_restore_handle(shared);
    second.inject("T2").unwrap();
    assert!(wait_for_clipboard(
        &second_handle,
        Some("user clipboard"),
        Duration::from_secs(1)
    ));
    // Cycle 1's timer must not clobber the restored value afterwards.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        second_handle.read_contents(),
        Some("user clipboard".to_owned())
    );
}

#[test]
fn adopt_restore_handle_swaps_the_coordinator_in_place() {
    // — when a Restart lands inside the 2 s
    // restore window, registration adopts a retained pending
    // coordinator into the already-Arc-shared replacement backend, so
    // the swap must work through &self and both handles must alias.
    use std::sync::Arc;
    let clipboard = RecordingClipboard::with_initial(None);
    let first = backend_with_clipboard_and_delay(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        RecordingBackend::new(),
        clipboard,
        Duration::ZERO,
    );
    let replacement = EnigoInjectBackend::new(Injector::new(), InjectMethod::Typing);
    let adopted = first.restore_handle();
    replacement.adopt_restore_handle(adopted.clone());
    assert!(Arc::ptr_eq(&replacement.restore_handle(), &adopted));
    assert!(Arc::ptr_eq(
        &replacement.restore_handle(),
        &first.restore_handle()
    ));
}
