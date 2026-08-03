use super::{model_download_status, model_download_warning, whisper_model_hint, WHISPER_MODELS};

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

#[test]
fn model_download_status_is_clear_for_both_states() {
    assert_eq!(model_download_status(true), "downloaded");
    assert_eq!(model_download_status(false), "not downloaded");
}

#[test]
fn unavailable_model_warns_before_recording() {
    assert_eq!(
        model_download_warning("large-v3", false).as_deref(),
        Some("large-v3 is not downloaded. Download it below before recording.")
    );
    assert_eq!(model_download_warning("large-v3", true), None);
}

#[test]
fn picker_offers_only_the_two_real_choices() {
    // The offer list is deliberately minimal (see WHISPER_MODELS): a machine
    // that cannot run these should use a cloud STT backend rather than a tiny
    // local model. Keep it in sync with the GGML download catalog so Settings
    // never offers a model the downloader won't fetch.
    assert_eq!(WHISPER_MODELS, &["large-v3-turbo", "large-v3"]);
}

#[test]
fn retired_models_keep_their_hints_for_saved_settings() {
    // Removing a model from the OFFER list must not blank out the picker for
    // someone who already had it selected — nothing resets `settings.model`,
    // so the hint lookup has to keep resolving the retired names.
    for retired in ["medium", "small", "base", "tiny"] {
        let (note, mb) = whisper_model_hint(retired);
        assert!(!note.is_empty(), "retired model {retired} lost its label");
        assert!(mb > 0, "retired model {retired} lost its VRAM estimate");
    }
}
