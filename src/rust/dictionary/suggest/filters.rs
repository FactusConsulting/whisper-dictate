//! Source-side noise filters for the replacement suggester.
//!
//! Sentence connectors ("the"/"og"/"med"), lone 1–2 letter tokens not on the
//! short allow-list, and a hand-curated phrase blacklist are all rejected so
//! the preview doesn't drown in noise. Also hosts the shared `words` /
//! `normalize` helpers since they are needed by the risky-source check and by
//! the matching code in sibling modules.

use std::sync::LazyLock;

use regex::Regex;

const COMMON_SOURCE_WORDS: &[&str] = &[
    "a", "an", "and", "as", "at", "be", "but", "by", "for", "from", "i", "in", "is", "it", "le",
    "of", "og", "on", "or", "skal", "the", "til", "to", "with", "de", "den", "det", "der", "du",
    "en", "et", "jeg", "kan", "med", "mig", "på", "så", "vi", "eller", "set", "fra", "type",
];

const COMMON_SOURCE_PHRASES: &[&str] = &[
    "begge",
    "begge forstå",
    "code",
    "code i",
    "consulting",
    "day",
    "kode",
    "large",
    "le code",
    "le terminal",
    "terminal",
    "two",
    "whisper",
    "claude",
    "consulting 2d",
    "contre celui",
    "dæv eller",
    "eller brød",
    "faktus consulting 2d",
    "faktus consulting og",
    "kom",
    "kobberites klosteret",
    "kodex versus",
    "køre",
    "køre klosteret",
    "large-v3 and",
    "mcp",
    "mcp rac",
    "pisit backend",
    "que",
    "serveren til remote lokal postprocessing",
    "serveren til remote lokal postprocessing.",
    "sit",
    "signal-to-noise-ratio tydelig i terminalen",
    "signal-to-noise-ratio tydelig i terminalen.",
    "typ",
    "voice pisit",
    "ændringen pudst",
    "ændringen pudste",
];

const SHORT_SOURCE_ALLOWLIST: &[&str] = &[
    "2d", "dbfs", "qn", "rac", "rag", "snr", "stt", "ui", "vad", "vlm", "xkb",
];

static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w.\-]+").unwrap());

pub(super) fn words(text: &str) -> Vec<String> {
    WORD_RE
        .find_iter(text)
        .map(|m| m.as_str().to_owned())
        .collect()
}

pub(super) fn normalize(text: &str) -> String {
    words(text).join(" ").to_lowercase()
}

fn is_common_source_word(token: &str) -> bool {
    COMMON_SOURCE_WORDS.contains(&token)
}

fn is_short_allowlisted(token: &str) -> bool {
    SHORT_SOURCE_ALLOWLIST.contains(&token)
}

fn is_common_source_phrase(phrase: &str) -> bool {
    COMMON_SOURCE_PHRASES.contains(&phrase)
}

pub(super) fn is_risky_source(source: &str) -> bool {
    let normalised = normalize(source);
    if normalised.is_empty() {
        return true;
    }
    let normalised_words: Vec<&str> = normalised.split_whitespace().collect();
    if is_common_source_phrase(&normalised) {
        return true;
    }
    if normalised_words.len() == 1 {
        let word = normalised_words[0];
        return is_common_source_word(word)
            || (word.chars().count() <= 2 && !is_short_allowlisted(word));
    }
    if is_common_source_word(normalised_words[0])
        || is_common_source_word(normalised_words[normalised_words.len() - 1])
    {
        return true;
    }
    if normalised_words.len() <= 3 && normalised_words.iter().any(|w| is_common_source_word(w)) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_is_risky() {
        assert!(is_risky_source(""));
        assert!(is_risky_source("   "));
    }

    #[test]
    fn common_single_word_is_risky_but_dictionary_term_is_not() {
        assert!(is_risky_source("the"));
        assert!(is_risky_source("og"));
        assert!(!is_risky_source("kubernetes"));
    }

    #[test]
    fn short_two_letter_is_risky_unless_allowlisted() {
        assert!(is_risky_source("ab"));
        assert!(!is_risky_source("ui"));
        assert!(!is_risky_source("rag"));
    }

    #[test]
    fn phrase_starting_or_ending_with_common_word_is_risky() {
        assert!(is_risky_source("the dictionary"));
        assert!(is_risky_source("dictionary the"));
        assert!(!is_risky_source("Kubernetes cluster"));
    }

    #[test]
    fn curated_phrase_blacklist_is_risky() {
        assert!(is_risky_source("le code"));
        assert!(is_risky_source("mcp rac"));
    }

    #[test]
    fn words_extracts_dotted_and_hyphenated_tokens() {
        assert_eq!(words("large-v3 and"), vec!["large-v3", "and"]);
        assert_eq!(words("foo.bar baz"), vec!["foo.bar", "baz"]);
    }

    #[test]
    fn normalize_lowercases_and_joins() {
        assert_eq!(normalize("Foo  BAR\nbaz"), "foo bar baz");
    }
}
