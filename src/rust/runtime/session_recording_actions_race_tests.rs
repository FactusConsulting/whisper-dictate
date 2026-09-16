//! Races between a slow microphone open and the next coordinator event
//! (#323): release, cancel or a second toggle press queued while the driver
//! is still opening must yield exactly one open, zero open streams at the
//! end, and transcription with the microphone already closed. Also covers
//! coordinator shutdown in the middle of a recording.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::{coordinator, env_lock, rig, SR};
use crate::dictate::SessionState;
use crate::hotkey::coordinator::{spawn as spawn_coordinator, CoordinatorEvent, Mode, Options};
use crate::runtime::capture_test_support::{wait_until, FakeFailure, FakeOpener};
use crate::runtime::live_settings::LiveEnvOverrides;
use crate::runtime::rust_session_sink::{
    build_session_action_sink_with_live_overrides, coordinator_signals, CoordinatorLink,
};

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
fn an_abort_raised_before_the_handle_is_published_is_applied_on_publication() {
    let link = Arc::new(CoordinatorLink::new());
    let signals = coordinator_signals(&link);

    // The listener is live but the supervisor has not published the handle
    // yet: this abort must not be dropped. A toggle release produces no
    // signal at all, so it cannot wait for the next one either.
    (signals.recording_abandoned)(1);

    let (handle, thread) = spawn_coordinator(
        Options {
            mode: Mode::Toggle,
            auto_complete_processing: false,
        },
        |_action| {},
        Instant::now,
    );
    assert!(
        link.publish(handle.clone()),
        "the first publication must win"
    );
    assert_eq!(
        handle.abort_recording_pending(),
        Some(1),
        "publishing the handle must raise the retained abort for that recording"
    );

    handle.shutdown();
    thread.join();
}

/// The genuine failed-start sequence with no injected signals: the first
/// toggle press fails to open before the supervisor published the handle,
/// the release emits nothing at all, and the next press must still open the
/// microphone instead of being consumed as a stop.
#[test]
fn a_failed_first_press_before_publication_lets_the_next_press_open_the_mic() {
    let _env = env_lock();
    let rig = rig(None);
    rig.opener.fail("", FakeFailure::Error);

    let link = Arc::new(CoordinatorLink::new());
    let (tx, _rx) = mpsc::channel();
    let sink = build_session_action_sink_with_live_overrides(
        Arc::clone(&rig.session),
        tx,
        coordinator_signals(&link),
        None,
        LiveEnvOverrides::default(),
        false,
        Some(Arc::clone(&rig.capture)),
    );
    let (coord, thread) = spawn_coordinator(
        Options {
            mode: Mode::Toggle,
            auto_complete_processing: false,
        },
        sink,
        Instant::now,
    );

    coord.send(CoordinatorEvent::Press);
    wait_until("the first open failed", || rig.opener.opened().len() == 1);
    // Toggle mode: the release produces no action, so no sink signal exists
    // that could carry the retained abort.
    coord.send(CoordinatorEvent::Release);
    assert!(
        link.publish(coord.clone()),
        "the supervisor publishes the handle after the listener is live"
    );
    rig.opener.clear_failure("");

    coord.send(CoordinatorEvent::Press);
    wait_until("the retry press opens the microphone", || {
        rig.opener.open_streams() == 1
    });
    assert_eq!(rig.opener.opened().len(), 2);
    assert!(
        rig.seen.lock().unwrap().is_empty(),
        "the retry press must not be consumed as a stop"
    );

    coord.send(CoordinatorEvent::Press);
    wait_until("the following press ends the recording", || {
        rig.opener.open_streams() == 0
    });
    assert_eq!(rig.session.lock().unwrap().state(), SessionState::Idle);

    coord.shutdown();
    thread.join();
}

/// The abort and the handle publication race each other on two threads.
/// Whichever order the two critical sections take, the abort must end up on
/// the coordinator: retained then flushed by `publish`, or delivered
/// straight to the already-published handle.
#[test]
fn an_abort_racing_publication_always_reaches_the_coordinator() {
    for _ in 0..50 {
        let link = Arc::new(CoordinatorLink::new());
        let signals = coordinator_signals(&link);
        let (handle, thread) = spawn_coordinator(
            Options {
                mode: Mode::Toggle,
                auto_complete_processing: false,
            },
            |_action| {},
            Instant::now,
        );

        let aborting_link = Arc::clone(&link);
        let abort = std::thread::spawn(move || {
            let signals = coordinator_signals(&aborting_link);
            (signals.recording_abandoned)(1);
        });
        assert!(link.publish(handle.clone()));
        abort.join().expect("abort thread");
        drop(signals);

        assert_eq!(
            handle.abort_recording_pending(),
            Some(1),
            "an abort must never be stranded by the publication race"
        );
        handle.shutdown();
        thread.join();
    }
}

/// A retained abort names the recording it belongs to, so it must never
/// reset a LATER recording: that one's microphone is open and only its own
/// stop or cancel may close it. Sequence: the first open fails before
/// publication, a bare-modifier chord cancel returns the coordinator to
/// idle, the next press opens successfully, and only then is the handle
/// published (raising the stale abort).
#[test]
fn a_stale_retained_abort_leaves_a_newer_recording_and_its_microphone_alone() {
    let _env = env_lock();
    let rig = rig(None);
    rig.opener.fail("", FakeFailure::Error);

    let link = Arc::new(CoordinatorLink::new());
    let (tx, _rx) = mpsc::channel();
    let sink = build_session_action_sink_with_live_overrides(
        Arc::clone(&rig.session),
        tx,
        coordinator_signals(&link),
        None,
        LiveEnvOverrides::default(),
        false,
        Some(Arc::clone(&rig.capture)),
    );
    let (coord, thread) = spawn_coordinator(
        Options {
            mode: Mode::HoldToTalk,
            auto_complete_processing: false,
        },
        sink,
        Instant::now,
    );

    // 1. First press fails to open while the handle is still unpublished.
    coord.send(CoordinatorEvent::Press);
    wait_until("the first open failed", || rig.opener.opened().len() == 1);
    assert_eq!(rig.opener.open_streams(), 0);

    // 2. A bare-modifier chord cancel returns the coordinator to idle.
    coord.send(CoordinatorEvent::Cancel);

    // 3. The next press opens a NEW recording successfully.
    rig.opener.clear_failure("");
    coord.send(CoordinatorEvent::Press);
    wait_until("the second press opened the microphone", || {
        rig.opener.open_streams() == 1
    });

    // 4. Publication now raises the abort retained from recording 1.
    assert!(link.publish(coord.clone()));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        rig.opener.open_streams(),
        1,
        "the stale abort must not touch the recording that is running"
    );

    // 5. The live recording's own release still closes the microphone.
    coord.send(CoordinatorEvent::Release);
    wait_until("the release closes the microphone", || {
        rig.opener.open_streams() == 0
    });
    assert_eq!(
        rig.opener.opened().len(),
        2,
        "exactly two opens: the failed first one and the live second one"
    );
    // The microphone closes before transcription runs, so the session settles
    // to Idle a moment after the stream count drops.
    wait_until("the session settles after the release", || {
        rig.session.lock().unwrap().state() == SessionState::Idle
    });

    coord.shutdown();
    thread.join();
    assert_eq!(
        rig.opener.open_streams(),
        0,
        "no capture stream may outlive the recording"
    );
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
