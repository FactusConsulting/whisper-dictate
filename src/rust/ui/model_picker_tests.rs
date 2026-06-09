use super::{whisper_model_hint, WHISPER_MODELS};

#[test]
fn every_whisper_model_has_a_nonempty_hint() {
    // Adding a model to WHISPER_MODELS without metadata would silently show it
    // with no accuracy note and a 0 MB estimate (so it never greys out).
    for model in WHISPER_MODELS {
        let (note, mb) = whisper_model_hint(model);
        assert!(!note.is_empty(), "missing accuracy note for {model}");
        assert!(mb > 0, "missing VRAM estimate for {model}");
    }
}

#[test]
fn unknown_model_has_empty_hint() {
    assert_eq!(whisper_model_hint("nonexistent"), ("", 0));
}
