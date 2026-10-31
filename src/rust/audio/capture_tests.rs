use super::*;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

/// The xrun notice runs on the cpal callback thread: with the diagnostic
/// writer installed and another thread holding the process stderr lock, the
/// report must still return promptly and the line must reach the log file.
#[test]
fn xrun_report_reaches_the_diag_log_without_waiting_on_stderr() {
    let _diag_lock = crate::diag_test_lock::lock_diag_writer();
    crate::diag::reset_level_for_tests();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("xrun.log");
    crate::diag::install_gui_diagnostic_log(&path).expect("install diag log");
    crate::diag::ensure_async_writer();

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

    crate::diag::flush_async_for_tests();
    let contents = std::fs::read_to_string(&path).expect("read diag log");
    assert!(
        contents.contains("[audio/capture] nonterminal cpal stream xrun"),
        "the xrun notice must reach the diagnostic log; got {contents:?}"
    );
}
