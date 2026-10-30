use super::*;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

/// The xrun notice runs on the cpal callback thread, so it must not wait on
/// the process stderr lock while another thread holds it.
#[test]
fn xrun_report_does_not_wait_on_a_held_stderr_lock() {
    let (locked_tx, locked_rx) = std_mpsc::channel::<()>();
    let (release_tx, release_rx) = std_mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let stderr = std::io::stderr();
        let _guard = stderr.lock();
        locked_tx.send(()).expect("signal lock held");
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });
    locked_rx.recv().expect("stderr lock held");

    let (done_tx, done_rx) = std_mpsc::channel::<()>();
    let reporter = std::thread::spawn(move || {
        let (tx, _rx) = audio_chunk_channel_with_capacity(1);
        let terminal = AtomicBool::new(false);
        enqueue_stream_error(&tx, &terminal, cpal::Error::new(cpal::ErrorKind::Xrun));
        let _ = done_tx.send(());
    });

    let returned = done_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    let _ = release_tx.send(());
    holder.join().expect("stderr holder");
    reporter.join().expect("xrun reporter");
    assert!(
        returned,
        "the xrun report blocked on the stderr lock held by another thread"
    );
}
