//! Races between a slow microphone open and the next coordinator event
//! (#323): release, cancel or a second toggle press queued while the driver
//! is still opening must yield exactly one open, zero open streams at the
//! end, and transcription with the microphone already closed. Also covers
//! coordinator shutdown in the middle of a recording.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{coordinator, env_lock, rig, SR};
use crate::dictate::SessionState;
use crate::hotkey::coordinator::{
    spawn as spawn_coordinator, CoordinatorEvent, CoordinatorHandle, Mode, Options,
};
use crate::runtime::capture_test_support::{wait_until, FakeFailure, FakeOpener};
use crate::runtime::rust_session_sink::coordinator_signals;

/// Make every open block until the returned sender is dropped (or sent a
/// unit). The counter tracks opens that reached the driver.
fn slow_opens(opener: &FakeOpener) -> (mpsc::Sender<()>, Arc<AtomicUsize>) {
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let entered = Arc::new(AtomicUsize::new(0));
    let entered_hook = Arc::clone(&entered);
    opener.on_open(move || {
        entered_hook.fetch_add(1, Ordering::SeqCst);
        let _ = gate.lock().unwrap().recv();
    });
    (release, entered)
}

#[test]
fn release_during_a_slow_open_closes_once_the_open_completes() {
    let _env = env_lock();
    let rig = rig(None);
    rig.opener.deliver_on_open(vec![0.1; SR]);
    let (release_open, entered) = slow_opens(&rig.opener);
    let (coord, thread) = coordinator(&rig.session, Arc::clone(&rig.capture), Mode::HoldToTalk);

    coord.send(CoordinatorEvent::Press);
    wait_until("open reached the driver", || {
        entered.load(Ordering::SeqCst) == 1
    });
    coord.send(CoordinatorEvent::Release);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(rig.opener.open_streams(), 0);
    drop(release_open);

    wait_until("release transcribes", || {
        rig.seen.lock().unwrap().len() == 1
    });
    assert_eq!(rig.seen.lock().unwrap()[0], (SR, 0));
    wait_until("mic closed", || rig.opener.open_streams() == 0);
    assert_eq!(rig.opener.opened().len(), 1, "exactly one open");

    coord.shutdown();
    thread.join();
}

#[test]
fn cancel_during_a_slow_open_closes_and_discards() {
    let _env = env_lock();
    let rig = rig(None);
    rig.opener.deliver_on_open(vec![0.1; SR]);
    let (release_open, entered) = slow_opens(&rig.opener);
    let (coord, thread) = coordinator(&rig.session, Arc::clone(&rig.capture), Mode::HoldToTalk);

    coord.send(CoordinatorEvent::Press);
    wait_until("open reached the driver", || {
        entered.load(Ordering::SeqCst) == 1
    });
    coord.send(CoordinatorEvent::Cancel);
    drop(release_open);

    wait_until("cancel settles with the mic closed", || {
        rig.session.lock().unwrap().state() == SessionState::Idle
            && rig.opener.open_streams() == 0
            && rig.opener.opened().len() == 1
    });
    assert!(rig.seen.lock().unwrap().is_empty(), "cancel discards");

    coord.shutdown();
    thread.join();
    assert_eq!(rig.opener.open_streams(), 0);
}

#[test]
fn toggle_double_press_during_a_slow_open_opens_once_and_closes() {
    let _env = env_lock();
    let rig = rig(None);
    rig.opener.deliver_on_open(vec![0.1; SR]);
    let (release_open, entered) = slow_opens(&rig.opener);
    let (coord, thread) = coordinator(&rig.session, Arc::clone(&rig.capture), Mode::Toggle);

    coord.send(CoordinatorEvent::Press);
    wait_until("open reached the driver", || {
        entered.load(Ordering::SeqCst) == 1
    });
    coord.send(CoordinatorEvent::Press);
    drop(release_open);

    wait_until("second press transcribes", || {
        rig.seen.lock().unwrap().len() == 1
    });
    assert_eq!(rig.seen.lock().unwrap()[0], (SR, 0));
    wait_until("mic closed", || rig.opener.open_streams() == 0);
    assert_eq!(rig.opener.opened().len(), 1, "exactly one open");

    coord.shutdown();
    thread.join();
}

#[test]
fn toggle_press_after_a_failed_open_opens_the_microphone_again() {
    let _env = env_lock();
    let rig = rig(None);
    rig.opener.fail("", FakeFailure::Error);
    let (coord, thread) = coordinator(&rig.session, Arc::clone(&rig.capture), Mode::Toggle);

    // The first open blocks (so the retry press is queued while the device is
    // still opening) and then fails; every later open succeeds.
    let (release_open, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let entered = Arc::new(AtomicUsize::new(0));
    let entered_hook = Arc::clone(&entered);
    let opener_for_hook = rig.opener.clone();
    rig.opener.on_open(move || {
        if entered_hook.fetch_add(1, Ordering::SeqCst) == 0 {
            let _ = gate.lock().unwrap().recv();
        } else {
            opener_for_hook.clear_failure("");
        }
    });

    coord.send(CoordinatorEvent::Press);
    wait_until("the failing open reached the driver", || {
        entered.load(Ordering::SeqCst) == 1
    });
    // Queued WHILE the failing open is still in flight: the abort must be
    // applied before this press is read, or toggle mode consumes it as a stop.
    coord.send(CoordinatorEvent::Press);
    drop(release_open);

    wait_until("the queued retry opens the microphone", || {
        rig.opener.open_streams() == 1
    });
    assert_eq!(rig.opener.opened().len(), 2);
    assert!(
        rig.seen.lock().unwrap().is_empty(),
        "the retry press must not be consumed as a stop"
    );

    coord.send(CoordinatorEvent::Press);
    wait_until("the third press ends the recording", || {
        rig.opener.open_streams() == 0
    });
    assert_eq!(rig.session.lock().unwrap().state(), SessionState::Idle);

    coord.shutdown();
    thread.join();
}

#[test]
fn an_abort_raised_before_the_handle_is_published_is_retained() {
    let slot: Arc<OnceLock<CoordinatorHandle>> = Arc::new(OnceLock::new());
    let signals = coordinator_signals(&slot);

    // The listener is live but the supervisor has not published the handle
    // yet: this abort must not be dropped.
    (signals.recording_abandoned)(1);

    let (handle, thread) = spawn_coordinator(
        Options {
            mode: Mode::Toggle,
            auto_complete_processing: false,
        },
        |_action| {},
        Instant::now,
    );
    assert!(slot.set(handle.clone()).is_ok());
    assert!(
        !handle.abort_recording_pending(),
        "the abort could not have reached the coordinator yet"
    );

    (signals.processing_finished)(1);
    assert!(
        handle.abort_recording_pending(),
        "the retained abort must reach the coordinator at the next signal"
    );

    handle.shutdown();
    thread.join();
}

#[test]
fn coordinator_shutdown_mid_recording_closes_the_microphone() {
    let _env = env_lock();
    let rig = rig(None);
    let opener = rig.opener.clone();
    let (coord, thread) = coordinator(&rig.session, rig.capture, Mode::HoldToTalk);

    coord.send(CoordinatorEvent::Press);
    wait_until("press opens the mic", || opener.open_streams() == 1);
    coord.shutdown();
    thread.join();

    assert_eq!(
        opener.open_streams(),
        0,
        "dropping the sink drops the lifecycle, which closes the stream"
    );
}
