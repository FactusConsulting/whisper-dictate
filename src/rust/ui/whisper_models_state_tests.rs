use super::*;

#[test]
fn cancellation_marks_only_the_active_download() {
    let state = WhisperModelDownloads::new();
    assert!(!state.cancel("tiny.en"));

    assert!(state.start("tiny.en"));
    assert!(state.cancel("tiny.en"));
    assert!(state
        .cancellation("tiny.en")
        .expect("active job has a token")
        .is_cancelled());

    state.finish_cancelled("tiny.en");
    assert!(!state.cancel("tiny.en"));
}

#[test]
fn cancelled_download_cannot_retry_until_its_worker_exits() {
    let state = WhisperModelDownloads::new();
    assert!(state.start("tiny.en"));
    let cancellation = state.cancellation("tiny.en").expect("token");
    let worker = cancellation.worker();

    assert!(state.cancel("tiny.en"));
    state.finish_cancelled("tiny.en");
    assert!(!state.can_start("tiny.en"));
    assert!(!state.start("tiny.en"));

    drop(worker);
    assert!(state.can_start("tiny.en"));
    assert!(state.start("tiny.en"));
}
