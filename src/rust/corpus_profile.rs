//! Pure corpus filter-profiles: select a subset of corpus items by
//! language/category. Rust port of `vp_corpus_profile.py` (Wave 6 follow-up to
//! the dictionary-training CLI port — see `dictionary/training/cli.rs`).
//!
//! The golden corpus (`benchmark/corpus.json`) tags every item with a `language`
//! (`da` / `en`) and a fine-grained `category` (`mixed_technical`,
//! `product_names`, `short_danish` …). Both the benchmark and the
//! corpus→dictionary training want to target a SUBSET — e.g. "only the Danish
//! technical items" — without the caller memorising the exact category strings.
//!
//! This module is PURE (no IO, no model load): it operates on already-parsed
//! corpus items via the [`HasLanguageAndCategory`] trait and never touches
//! audio. Two layers:
//!
//! * a small named-group vocabulary (`CATEGORY_GROUPS`) mapping friendly group
//!   names (`technical`, `business`, `names` …) onto the concrete categories in
//!   the shipped corpus, so `--category technical` works without the user
//!   knowing it expands to `mixed_technical`/`english_technical`/`terminal`;
//! * [`filter_corpus_items`], which applies an optional language filter AND an
//!   optional category/group filter (case-insensitive, multi-value), defaulting
//!   to "all items" so existing benchmark behaviour is unchanged when no
//!   profile is given.

use std::collections::HashSet;

/// Structural trait for the bits of a corpus item this module reads. Declared
/// as a trait so the pure filter works against the real [`crate::corpus::CorpusItem`]
/// AND lightweight test doubles without an import cycle.
pub trait HasLanguageAndCategory {
    fn language(&self) -> &str;
    fn category(&self) -> &str;
}

/// Friendly category-group names → the concrete `category` strings that appear
/// in the shipped `benchmark/corpus.json`. A group expands to its members so a
/// profile like `technical` selects every technical-flavoured category without
/// the caller naming each one. Group names and members are matched
/// case-insensitively (see [`expand_categories`]). Unknown categories simply
/// never match a group (and can still be selected by their exact name).
pub const CATEGORY_GROUPS: &[(&str, &[&str])] = &[
    // Technical / engineering-flavoured items (commands, infra, code talk).
    (
        "technical",
        &["mixed_technical", "english_technical", "terminal"],
    ),
    // Product / brand / people names that must keep exact spelling.
    ("business", &["product_names", "names"]),
    ("names", &["product_names", "names"]),
    ("products", &["product_names"]),
    // Short utterances in either language.
    ("short", &["short_danish", "short_english"]),
    // Long-form paragraphs.
    ("long", &["long_danish"]),
    // UI / workflow phrasing.
    ("ui", &["ui_workflow"]),
    ("workflow", &["ui_workflow"]),
    // Robustness buckets.
    ("noise", &["noise_sensitive"]),
    ("punctuation", &["punctuation"]),
    ("layout", &["layout"]),
    ("mixed", &["mixed_language", "mixed_technical"]),
];

fn lookup_group(token: &str) -> Option<&'static [&'static str]> {
    CATEGORY_GROUPS
        .iter()
        .find(|(name, _)| *name == token)
        .map(|(_, members)| *members)
}

/// A normalised selection over the corpus: optional languages + categories.
/// Both filters are vectors of casefolded tokens. Empty means "no constraint
/// on this axis" so an empty profile (the default) selects every item.
/// `categories` holds concrete corpus category strings AFTER group expansion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorpusProfile {
    pub languages: Vec<String>,
    pub categories: Vec<String>,
}

impl CorpusProfile {
    /// True when neither axis constrains anything (selects all items).
    pub fn is_empty(&self) -> bool {
        self.languages.is_empty() && self.categories.is_empty()
    }

    /// Human-readable one-liner for CLI preview / errors (never raises).
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "all items".to_owned();
        }
        let mut parts: Vec<String> = Vec::new();
        if !self.languages.is_empty() {
            parts.push(format!("language={}", self.languages.join("/")));
        }
        if !self.categories.is_empty() {
            // Sort categories for stable output (matches Python `sorted(...)`).
            let mut cats = self.categories.clone();
            cats.sort();
            parts.push(format!("category={}", cats.join("/")));
        }
        parts.join(", ")
    }
}

/// Normalise a raw selector into a vec of casefolded, non-empty tokens.
/// Accepts `None` or a comma-separated string (`"da,en"`), mirroring Python's
/// `_split_tokens`. Whitespace is trimmed and empties dropped; order is
/// preserved minus dupes.
pub fn split_tokens(value: Option<&str>) -> Vec<String> {
    let raw = match value {
        Some(v) => v,
        None => return Vec::new(),
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let norm = part.trim().to_lowercase();
        if !norm.is_empty() && seen.insert(norm.clone()) {
            out.push(norm);
        }
    }
    out
}

/// Expand category/group tokens into concrete corpus category strings. Each
/// token is looked up in [`CATEGORY_GROUPS`] (case-insensitively); a group
/// expands to its members, while a non-group token is kept verbatim so an
/// exact category name (`mixed_technical`) still works. Results are casefolded
/// and de-duplicated, preserving first-seen order.
pub fn expand_categories(tokens: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for token in tokens {
        let expanded: Vec<String> = match lookup_group(token) {
            Some(members) => members.iter().map(|m| (*m).to_owned()).collect(),
            None => vec![token.clone()],
        };
        for category in expanded {
            let norm = category.trim().to_lowercase();
            if !norm.is_empty() && seen.insert(norm.clone()) {
                out.push(norm);
            }
        }
    }
    out
}

/// Build a normalised [`CorpusProfile`] from raw CLI selectors. `language` and
/// `category` each accept a comma-separated string; categories are
/// group-expanded via [`CATEGORY_GROUPS`]. All-empty inputs yield an empty
/// profile (selects everything), preserving the default benchmark / training
/// behaviour.
pub fn build_profile(language: Option<&str>, category: Option<&str>) -> CorpusProfile {
    let languages = split_tokens(language);
    let categories = expand_categories(&split_tokens(category));
    CorpusProfile {
        languages,
        categories,
    }
}

/// Whether a single corpus item satisfies the profile (pure predicate). Empty
/// axes never exclude. Language and category are matched case-insensitively
/// against the item's own `language` / `category`. Both axes are ANDed.
pub fn matches_profile<T: HasLanguageAndCategory>(item: &T, profile: &CorpusProfile) -> bool {
    if profile.is_empty() {
        return true;
    }
    if !profile.languages.is_empty() {
        let lang = item.language().trim().to_lowercase();
        if !profile.languages.iter().any(|l| l == &lang) {
            return false;
        }
    }
    if !profile.categories.is_empty() {
        let category = item.category().trim().to_lowercase();
        if !profile.categories.iter().any(|c| c == &category) {
            return false;
        }
    }
    true
}

/// Return the subset of `items` matching `profile` (order preserved). The
/// single entry point used by the training CLI; with an empty profile this
/// returns every item unchanged.
pub fn filter_corpus_items<T: HasLanguageAndCategory + Clone>(
    items: &[T],
    profile: &CorpusProfile,
) -> Vec<T> {
    items
        .iter()
        .filter(|item| matches_profile(*item, profile))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct Item {
        language: String,
        category: String,
    }

    impl HasLanguageAndCategory for Item {
        fn language(&self) -> &str {
            &self.language
        }
        fn category(&self) -> &str {
            &self.category
        }
    }

    fn item(language: &str, category: &str) -> Item {
        Item {
            language: language.to_owned(),
            category: category.to_owned(),
        }
    }

    #[test]
    fn split_tokens_dedupes_and_trims() {
        let tokens = split_tokens(Some("da, en, DA , "));
        assert_eq!(tokens, vec!["da", "en"]);
    }

    #[test]
    fn split_tokens_none_yields_empty() {
        assert!(split_tokens(None).is_empty());
    }

    #[test]
    fn expand_categories_replaces_group_with_members() {
        let expanded = expand_categories(&["technical".to_owned()]);
        assert!(expanded.contains(&"mixed_technical".to_owned()));
        assert!(expanded.contains(&"english_technical".to_owned()));
        assert!(expanded.contains(&"terminal".to_owned()));
    }

    #[test]
    fn expand_categories_keeps_unknown_token_verbatim() {
        let expanded = expand_categories(&["mixed_technical".to_owned()]);
        assert_eq!(expanded, vec!["mixed_technical"]);
    }

    #[test]
    fn empty_profile_selects_everything() {
        let items = vec![item("da", "short_danish"), item("en", "product_names")];
        let profile = CorpusProfile::default();
        assert!(profile.is_empty());
        assert_eq!(filter_corpus_items(&items, &profile).len(), 2);
        assert_eq!(profile.describe(), "all items");
    }

    #[test]
    fn language_filter_restricts() {
        let items = vec![
            item("da", "short_danish"),
            item("en", "short_english"),
            item("DA", "names"),
        ];
        let profile = build_profile(Some("da"), None);
        let kept = filter_corpus_items(&items, &profile);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn category_group_filter_expands() {
        let items = vec![
            item("da", "mixed_technical"),
            item("en", "english_technical"),
            item("en", "names"),
        ];
        let profile = build_profile(None, Some("technical"));
        let kept = filter_corpus_items(&items, &profile);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn both_axes_anded() {
        let items = vec![
            item("da", "mixed_technical"),
            item("en", "english_technical"),
        ];
        let profile = build_profile(Some("da"), Some("technical"));
        let kept = filter_corpus_items(&items, &profile);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].language, "da");
    }

    #[test]
    fn describe_lists_language_and_category() {
        let profile = build_profile(Some("da,en"), Some("product_names"));
        let described = profile.describe();
        assert!(described.contains("language=da/en"));
        assert!(described.contains("category=product_names"));
    }

    #[test]
    fn describe_sorts_categories_alphabetically() {
        // The "names" group expands to two categories; describe sorts them.
        let profile = build_profile(None, Some("names"));
        let described = profile.describe();
        assert!(described.contains("category=names/product_names"));
    }

    #[test]
    fn unknown_language_matches_zero_items() {
        let items = vec![item("da", "short_danish"), item("en", "short_english")];
        let profile = build_profile(Some("fr"), None);
        assert!(!profile.is_empty());
        assert!(filter_corpus_items(&items, &profile).is_empty());
    }
}
