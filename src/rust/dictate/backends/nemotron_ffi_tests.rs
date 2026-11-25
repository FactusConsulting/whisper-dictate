use super::speech_phrase_values;

#[test]
fn dictionary_terms_are_individual_native_speech_context_phrases() {
    let terms = vec!["Codex".to_owned(), "Cloudflare".to_owned()];

    assert_eq!(
        speech_phrase_values(Some("Vocabulary: Codex, Cloudflare"), &terms),
        vec!["Codex", "Cloudflare"]
    );
}

#[test]
fn base_prompt_is_preserved_beside_individual_dictionary_phrases() {
    let terms = vec!["Codex".to_owned(), "Cloudflare".to_owned()];

    assert_eq!(
        speech_phrase_values(
            Some("Project Aurora\nVocabulary: Codex, Cloudflare"),
            &terms,
        ),
        vec!["Project Aurora", "Codex", "Cloudflare"]
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
fn custom_prompt_is_preserved_when_dictionary_terms_are_present() {
    let terms = vec!["Kubernetes".to_owned(), "Cloudflare".to_owned()];
    assert_eq!(
        speech_phrase_values(Some("Kubernetes, Cloudflare"), &terms),
        vec!["Kubernetes, Cloudflare", "Kubernetes", "Cloudflare"]
    );
}

#[test]
fn custom_prompt_ending_in_vocabulary_words_is_not_mistaken_for_generated_suffix() {
    let terms = vec!["Codex".to_owned(), "Cloudflare".to_owned()];

    assert_eq!(
        speech_phrase_values(Some("Discuss Vocabulary: Codex, Cloudflare"), &terms),
        vec![
            "Discuss Vocabulary: Codex, Cloudflare",
            "Codex",
            "Cloudflare"
        ]
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

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn fixture_library_exercises_the_dynamic_abi_end_to_end() {
    use super::{NativeApi, NativeRecognizer};

    let directory = tempfile::tempdir().expect("temporary native ABI fixture directory");
    let library = super::build_fixture_library(directory.path());
    let model = directory.path().join("fixture.gguf");
    std::fs::write(&model, b"fixture model").expect("write fixture model");

    let api = NativeApi::load(&library).expect("load native fixture library");
    let error = NativeRecognizer::create(api, &model, -1)
        .err()
        .expect("the adapter itself rejects CPU");
    assert!(error.to_string().contains("CPU inference is not allowed"));

    let api = NativeApi::load(&library).expect("load native fixture library");
    let recognizer =
        NativeRecognizer::create(api, &model, 0).expect("load fixture native recognizer");
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
