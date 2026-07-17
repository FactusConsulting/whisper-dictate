//! Pure corpus → dictionary training helpers. This is the shipping
//! implementation — the Python `vp_dictionary_training.py` parity was
//! retired in audit item 4 (`docs/architecture-audit-2026-07-16.md`).
//!
//! Split into sibling files to stay under the repo-wide ~500 LOC per-file
//! gate:
//!
//! * `extract` – mine proper nouns / technical tokens out of corpus reference
//!   text (capitalised tokens, multi-word capitalised runs, all-caps acronyms,
//!   digit-mixed identifiers, studly-caps product names). Curated terms are
//!   always kept.
//! * `merge` – `merge_terms`, the case-insensitive append helper that turns a
//!   candidate list into a [`MergePreview`] (added vs already present)
//!   without writing the file.
//! * `misses` – surface the domain terms the benchmark GOT WRONG (annotated
//!   `term_misses`) as SUGGESTED additions — preview-only, NEVER auto-applied.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub mod cli;
mod cli_report;
mod extract;
mod merge;
mod misses;

pub use cli::{
    run_build_from_corpus, run_suggest_from_misses, BuildFromCorpusOptions,
    SuggestFromMissesOptions,
};
pub use extract::extract_candidate_terms;
pub use merge::merge_terms;
pub use misses::{suggest_terms_from_misses, BenchmarkRow};

/// A proposed dictionary term plus *why* it was proposed (for the preview).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermCandidate {
    pub term: String,
    #[serde(default = "default_one")]
    pub count: usize,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub samples: Vec<String>,
}

/// A SUGGESTED dictionary term from benchmark misses (preview, confirm first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSuggestion {
    pub term: String,
    #[serde(default = "default_one")]
    pub count: usize,
    #[serde(default)]
    pub samples: Vec<String>,
    #[serde(default)]
    pub already_in_dictionary: bool,
}

/// Outcome of merging candidates into the existing dictionary (no IO done).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MergePreview {
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub skipped_existing: Vec<String>,
    #[serde(default)]
    pub result_terms: Vec<String>,
    #[serde(default)]
    pub existing_count: usize,
}

impl MergePreview {
    pub fn added_count(&self) -> usize {
        self.added.len()
    }
}

fn default_one() -> usize {
    1
}

pub(super) static WORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\wÀ-ɏ]+(?:[.\-][\wÀ-ɏ]+)*").unwrap());

pub(super) fn normalize(term: &str) -> String {
    term.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(super) fn words(text: &str) -> Vec<String> {
    WORD_RE
        .find_iter(text)
        .map(|m| m.as_str().to_owned())
        .collect()
}
