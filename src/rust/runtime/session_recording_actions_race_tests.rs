//! Races between a slow microphone open and the next coordinator event
//! (#323): release, cancel or a second toggle press queued while the driver
//! is still opening must yield exactly one open, zero open streams at the
//! end, and transcription with the microphone already closed. Also covers
//! coordinator shutdown in the middle of a recording.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use super::{coordinator, env_lock, rig, SR};
use crate::dictate::SessionState;
use crate::hotkey::coordinator::{CoordinatorEvent, Mode};
use crate::runtime::capture_test_support::{wait_until, FakeOpener};

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
