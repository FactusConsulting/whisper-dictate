use super::*;

use std::sync::{mpsc, Arc};
use std::time::Duration;

use tempfile::tempdir;

#[test]
fn cancelled_publication_lock_wait_removes_extracted_staging_tree() {
    let directory = tempdir().expect("temporary runtime publication directory");
    let destination = directory.path().join("runtime");
    let staging = directory.path().join("runtime-partial");
    let library_filename = if cfg!(windows) {
        "nemo_speech_asr_c.dll"
    } else {
        "libnemo_speech_asr_c.so"
    };
    fs::create_dir_all(&staging).expect("create runtime staging directory");
    let library = staging.join(library_filename);
    fs::write(&library, b"candidate runtime").expect("write candidate runtime");
    write_runtime_verification_marker(&staging, &library, TEST_ARCHIVE_SHA256)
        .expect("verify candidate staging");

    let holder = acquire_runtime_publish_lock(&destination, &AtomicBool::new(true))
        .expect("hold publication lock");
    let active = Arc::new(AtomicBool::new(true));
    let worker_active = Arc::clone(&active);
    let worker_staging = staging.clone();
    let worker_destination = destination.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).expect("signal publication waiter");
        let result = publish_runtime(
            &worker_staging,
            &worker_destination,
            library_filename,
            TEST_ARCHIVE_SHA256,
            false,
            &worker_active,
        );
        result_tx.send(result).expect("return publication result");
    });
    started_rx.recv().expect("publication waiter started");
    std::thread::sleep(Duration::from_millis(50));

    active.store(false, Ordering::Release);
    let error = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("cancelled publication must return")
        .expect_err("cancelled publication must fail");

    assert!(error.to_string().contains("publication cancelled"));
    assert!(!staging.exists(), "cancelled staging tree must be removed");
    drop(holder);
    worker.join().expect("publication waiter exits");
}
