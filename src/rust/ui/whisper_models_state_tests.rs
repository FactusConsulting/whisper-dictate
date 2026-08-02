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
