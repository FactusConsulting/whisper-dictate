use super::{lock_env, ENV_LOCK};
use crate::diag_test_lock::{lock_diag_writer, DIAG_WRITER_LOCK};

#[test]
fn lock_env_recovers_after_holder_panics() {
    let poisoner = std::thread::spawn(|| {
        let _guard = lock_env();
        panic!("poison ENV_LOCK on purpose");
    });
    assert!(poisoner.join().is_err());
    assert!(ENV_LOCK.is_poisoned());
    drop(lock_env());
}

#[test]
fn lock_diag_writer_recovers_after_holder_panics() {
    let poisoner = std::thread::spawn(|| {
        let _guard = lock_diag_writer();
        panic!("poison DIAG_WRITER_LOCK on purpose");
    });
    assert!(poisoner.join().is_err());
    assert!(DIAG_WRITER_LOCK.is_poisoned());
    drop(lock_diag_writer());
}
