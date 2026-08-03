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

fn assert_cancelled_download_blocks_start(source: &'static str, next: &'static str) {
    let state = WhisperModelDownloads::new();
    assert!(state.start(source));
    let cancellation = state.cancellation(source).expect("token");
    let worker = cancellation.worker();

    assert!(state.cancel(source));
    state.finish_cancelled(source);
    assert!(!state.can_start(next));
    assert!(!state.start(next));

    drop(worker);
    assert!(state.can_start(next));
    assert!(state.start(next));
}

#[test]
fn cancelled_download_cannot_retry_until_its_worker_exits() {
    assert_cancelled_download_blocks_start("tiny.en", "tiny.en");
}

#[test]
fn cancelled_download_blocks_other_model_starts_until_its_worker_exits() {
    assert_cancelled_download_blocks_start("large-v3", "large-v3-turbo");
}

#[test]
fn stalled_download_allows_a_retry_while_its_reader_unwinds() {
    let state = WhisperModelDownloads::new();
    assert!(state.start("large-v3"));
    let worker = state.cancellation("large-v3").expect("token").worker();

    state.finish_err("large-v3", "download stalled".to_owned());
    assert!(state.can_start("large-v3-turbo"));
    assert!(state.start("large-v3-turbo"));

    drop(worker);
}
