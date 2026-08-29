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
fn custom_prompt_is_used_when_dictionary_has_no_terms() {
    assert_eq!(
        speech_phrase_values(Some("Project Aurora"), &[]),
        vec!["Project Aurora"]
    );
}
