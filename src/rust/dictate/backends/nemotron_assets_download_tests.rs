use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

use tempfile::tempdir;

use super::*;

struct CancellingReader<'a> {
    active: &'a AtomicBool,
    yielded: bool,
}

impl std::io::Read for CancellingReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.yielded {
            return Ok(0);
        }
        self.yielded = true;
        buffer[..4].copy_from_slice(b"data");
        self.active
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(4)
    }
}

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
fn cached_verification_observes_cancellation_between_reads() {
    let active = AtomicBool::new(true);
    let reader = CancellingReader {
        active: &active,
        yielded: false,
    };

    let error = verify_reader_sha256_while(reader, &"0".repeat(64), &active, "model")
        .expect_err("verification must stop after lifecycle cancellation");

    assert!(error.to_string().contains("model verification cancelled"));
}

#[test]
fn delayed_model_repair_preserves_a_verified_process_winner() {
    let directory = tempdir().expect("temporary model publication directory");
    let target = directory.path().join("model.gguf");
    let partial = directory.path().join("model.partial");
    std::fs::write(&target, b"stale").expect("write stale model");
    std::fs::write(&partial, b"verified winner").expect("write candidate model");
    let expected = sha256_file(&partial).expect("hash candidate model");

    let held = acquire_asset_publish_lock(&target, &AtomicBool::new(true), "model")
        .expect("hold model publication lock");
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

#[test]
fn model_publication_lock_wait_observes_runtime_cancellation() {
    let directory = tempdir().expect("temporary model publication directory");
    let target = directory.path().join("model.gguf");
    let holder_active = AtomicBool::new(true);
    let held = acquire_asset_publish_lock(&target, &holder_active, "model")
        .expect("hold model publication lock");
    let active = std::sync::Arc::new(AtomicBool::new(true));
    let worker_active = std::sync::Arc::clone(&active);
    let worker_target = target.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).expect("signal lock waiter");
        let result = acquire_asset_publish_lock(&worker_target, &worker_active, "model");
        result_tx.send(result).expect("return lock result");
    });
    started_rx.recv().expect("lock waiter started");
    std::thread::sleep(Duration::from_millis(50));

    active.store(false, std::sync::atomic::Ordering::Release);
    let error = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("cancelled lock waiter must return")
        .expect_err("cancelled lock waiter must not acquire the lock");

    assert!(error.to_string().contains("cancelled"));
    drop(held);
    worker.join().expect("lock waiter exits");
}
