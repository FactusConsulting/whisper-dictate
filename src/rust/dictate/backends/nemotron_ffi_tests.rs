use std::path::Path;

use super::{
    library_is_loadable, platform_loader_fallback, speech_phrase_values, NativeRecognizer,
};

#[test]
fn dictionary_terms_are_individual_native_speech_context_phrases() {
    let terms = vec!["Codex".to_owned(), "Cloudflare".to_owned()];

    assert_eq!(
        speech_phrase_values(Some("Vocabulary: Codex, Cloudflare"), &terms),
        vec!["Codex", "Cloudflare"]
    );
}

#[test]
fn custom_prompt_is_used_when_dictionary_has_no_terms() {
    assert_eq!(
        speech_phrase_values(Some("Project Aurora"), &[]),
        vec!["Project Aurora"]
    );
}

#[test]
fn windows_does_not_use_a_bare_dll_loader_fallback() {
    if cfg!(windows) {
        assert!(platform_loader_fallback().is_none());
    } else {
        assert!(platform_loader_fallback().is_some());
    }
}

#[test]
fn dictionary_phrases_replace_the_composed_prompt() {
    let terms = vec!["Kubernetes".to_owned(), "Cloudflare".to_owned()];
    assert_eq!(
        speech_phrase_values(Some("Kubernetes, Cloudflare"), &terms),
        vec!["Kubernetes", "Cloudflare"]
    );
}

#[test]
fn raw_dictionary_phrases_are_used_without_a_prompt() {
    let terms = vec![
        " Kubernetes ".to_owned(),
        "".to_owned(),
        "Cloudflare".to_owned(),
    ];
    assert_eq!(
        speech_phrase_values(None, &terms),
        vec![" Kubernetes ", "Cloudflare"]
    );
}

#[test]
fn loader_probe_rejects_a_missing_soname() {
    assert!(!library_is_loadable(Path::new(
        "whisper-dictate-library-that-does-not-exist.so",
    )));
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn fixture_library_exercises_the_dynamic_abi_end_to_end() {
    let directory = tempfile::tempdir().expect("temporary native ABI fixture directory");
    let library = super::build_fixture_library(directory.path());
    let model = directory.path().join("fixture.gguf");
    std::fs::write(&model, b"fixture model").expect("write fixture model");

    let recognizer =
        NativeRecognizer::new(&library, &model, -1).expect("load fixture native recognizer");
    let result = recognizer
        .recognize(
            &[0.2, -0.2],
            16_000,
            "en-US",
            Some("Vocabulary: Codex"),
            &["Codex".to_owned()],
        )
        .expect("fixture recognition");
    assert_eq!(result.text, "fixture transcript");
    assert_eq!(result.language.as_deref(), Some("en-US"));

    let error = recognizer
        .recognize(&[], 16_000, "en-US", None, &[])
        .expect_err("fixture rejects empty audio");
    assert!(error.to_string().contains("fixture native error"));
}
