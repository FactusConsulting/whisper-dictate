use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

use tempfile::tempdir;

use super::*;

#[test]
fn hex_lower_encodes_each_nibble_in_lowercase() {
    assert_eq!(hex_lower(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
}

#[test]
fn stopped_runtime_rejects_download_work() {
    let stopped = AtomicBool::new(false);
    let error = ensure_runtime_active(&stopped, "model").expect_err("stopped runtime must cancel");

    assert!(error.to_string().contains("download cancelled"));
}

#[test]
fn delayed_model_repair_preserves_a_verified_process_winner() {
    let directory = tempdir().expect("temporary model publication directory");
    let target = directory.path().join("model.gguf");
    let partial = directory.path().join("model.partial");
    std::fs::write(&target, b"stale").expect("write stale model");
    std::fs::write(&partial, b"verified winner").expect("write candidate model");
    let expected = sha256_file(&partial).expect("hash candidate model");

    let held = acquire_asset_publish_lock(&target).expect("hold model publication lock");
    let (started_tx, started_rx) = mpsc::channel();
    let worker_target = target.clone();
    let worker_partial = partial.clone();
    let worker_expected = expected.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        publish_verified_file(&worker_partial, &worker_target, &worker_expected)
    });
    started_rx.recv().expect("repairer started");
    std::thread::sleep(Duration::from_millis(50));
    assert!(!worker.is_finished(), "repairer must wait for its peer");

    std::fs::write(&target, b"verified winner").expect("publish verified peer winner");
    drop(held);

    worker
        .join()
        .expect("delayed repairer exits")
        .expect("verified peer wins");
    assert_eq!(std::fs::read(&target).unwrap(), b"verified winner");
    assert!(!partial.exists(), "losing partial file is cleaned");
}
