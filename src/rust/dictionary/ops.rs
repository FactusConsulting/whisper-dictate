//! `dictionary-ops` JSON-RPC dispatcher (hidden CLI subcommand).
//!
//! The Python shell-out fallback (`VOICEPI_DICTIONARY_BACKEND=rust`) lives in
//! `vp_dictionary_training.py` and `vp_dictionary_suggest.py`. Rather than
//! adding one CLI verb per Python function, we expose a single
//! `dictionary-ops` subcommand that accepts a JSON envelope on stdin:
//!
//! ```json
//! { "op": "extract_candidate_terms", "params": { ... } }
//! ```
//!
//! and writes a JSON response on stdout. On any unrecognised op / malformed
//! input we exit non-zero with a structured error so the Python caller can
//! cleanly fall back to its in-process code path.

use std::io::{self, Read};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::suggest::{
    suggest_replacements_from_rows, DictionarySnapshot, ReplacementSuggestion, SuggestRow,
};
use super::training::{
    extract_candidate_terms, merge_terms, suggest_terms_from_misses, BenchmarkRow, MergePreview,
    TermCandidate, TermSuggestion,
};

#[derive(Debug, Deserialize)]
struct OpRequest {
    op: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct OpError {
    error: String,
}

/// Entry point wired into `main.rs`.
pub fn handle_ops() -> Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let request: OpRequest = serde_json::from_str(&raw)
        .map_err(|err| anyhow!("malformed dictionary-ops request: {err}"))?;
    let response = dispatch(&request.op, request.params)?;
    println!("{response}");
    Ok(())
}

fn dispatch(op: &str, params: Value) -> Result<String> {
    match op {
        "extract_candidate_terms" => json_response(extract_op(params)?),
        "merge_terms" => json_response(merge_op(params)?),
        "suggest_terms_from_misses" => json_response(suggest_terms_op(params)?),
        "suggest_replacements_from_rows" => json_response(suggest_replacements_op(params)?),
        other => {
            let err = OpError {
                error: format!("unknown dictionary op: {other}"),
            };
            // Print structured error so the Python caller can detect & fall back.
            println!("{}", serde_json::to_string(&err)?);
            Err(anyhow!("unknown dictionary op: {other}"))
        }
    }
}

fn json_response<T: Serialize>(value: T) -> Result<String> {
    Ok(serde_json::to_string(&value)?)
}

// ---------------------------------------------------------------- extract_op

#[derive(Debug, Deserialize)]
struct ExtractParams {
    #[serde(default)]
    texts: Vec<String>,
    #[serde(default)]
    item_terms: Option<Vec<Vec<String>>>,
    #[serde(default)]
    item_ids: Option<Vec<String>>,
    #[serde(default = "default_one")]
    min_count: usize,
}

#[derive(Debug, Serialize)]
struct ExtractResponse {
    candidates: Vec<TermCandidate>,
}

fn extract_op(params: Value) -> Result<ExtractResponse> {
    let params: ExtractParams = serde_json::from_value(params)?;
    let candidates = extract_candidate_terms(
        &params.texts,
        params.item_terms.as_deref(),
        params.item_ids.as_deref(),
        params.min_count,
    );
    Ok(ExtractResponse { candidates })
}

// ------------------------------------------------------------------ merge_op

#[derive(Debug, Deserialize)]
struct MergeParams {
    #[serde(default)]
    existing: Vec<String>,
    #[serde(default)]
    candidates: Vec<MergeCandidate>,
}

/// Accept both bare strings and `{"term": "..."}` shapes so the Python caller
/// can forward a candidate list verbatim.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MergeCandidate {
    Bare(String),
    Object {
        term: String,
        #[serde(default)]
        _count: Option<usize>,
        #[serde(default)]
        _reason: Option<String>,
    },
}

impl MergeCandidate {
    fn term(self) -> String {
        match self {
            Self::Bare(term) => term,
            Self::Object { term, .. } => term,
        }
    }
}

#[derive(Debug, Serialize)]
struct MergeResponse {
    #[serde(flatten)]
    preview: MergePreview,
    added_count: usize,
}

fn merge_op(params: Value) -> Result<MergeResponse> {
    let params: MergeParams = serde_json::from_value(params)?;
    let candidates: Vec<String> = params
        .candidates
        .into_iter()
        .map(MergeCandidate::term)
        .collect();
    let preview = merge_terms(&params.existing, &candidates);
    let added_count = preview.added_count();
    Ok(MergeResponse {
        preview,
        added_count,
    })
}

// ---------------------------------------------------- suggest_terms_from_misses

#[derive(Debug, Deserialize)]
struct SuggestTermsParams {
    #[serde(default)]
    rows: Vec<BenchmarkRow>,
    #[serde(default)]
    existing_terms: Vec<String>,
    #[serde(default = "default_one")]
    min_count: usize,
}

#[derive(Debug, Serialize)]
struct SuggestTermsResponse {
    suggestions: Vec<TermSuggestion>,
}

fn suggest_terms_op(params: Value) -> Result<SuggestTermsResponse> {
    let params: SuggestTermsParams = serde_json::from_value(params)?;
    let suggestions =
        suggest_terms_from_misses(&params.rows, &params.existing_terms, params.min_count);
    Ok(SuggestTermsResponse { suggestions })
}

// -------------------------------------------------- suggest_replacements_from_rows

#[derive(Debug, Deserialize)]
struct SuggestReplacementsParams {
    #[serde(default)]
    rows: Vec<SuggestRow>,
    #[serde(default)]
    dictionary: DictionarySnapshot,
    #[serde(default = "default_min_confidence")]
    min_confidence: f64,
}

#[derive(Debug, Serialize)]
struct SuggestReplacementsResponse {
    suggestions: Vec<ReplacementSuggestion>,
}

fn suggest_replacements_op(params: Value) -> Result<SuggestReplacementsResponse> {
    let params: SuggestReplacementsParams = serde_json::from_value(params)?;
    let suggestions =
        suggest_replacements_from_rows(&params.rows, &params.dictionary, params.min_confidence);
    Ok(SuggestReplacementsResponse { suggestions })
}

fn default_one() -> usize {
    1
}

fn default_min_confidence() -> f64 {
    0.62
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_op_round_trips_curated_terms() {
        let params = serde_json::json!({
            "texts": ["plain sentence"],
            "item_terms": [["Parakeet"]],
            "item_ids": ["a"],
            "min_count": 5
        });
        let response = extract_op(params).unwrap();
        assert!(response.candidates.iter().any(|c| c.term == "Parakeet"));
    }

    #[test]
    fn merge_op_accepts_bare_and_object_candidates() {
        let params = serde_json::json!({
            "existing": ["Old"],
            "candidates": ["New", {"term": "Another", "count": 2, "reason": "curated_term"}]
        });
        let response = merge_op(params).unwrap();
        assert_eq!(response.preview.added, vec!["New", "Another"]);
        assert_eq!(response.added_count, 2);
    }

    #[test]
    fn unknown_op_returns_error() {
        let response = dispatch("not_a_real_op", Value::Null);
        assert!(response.is_err());
    }

    #[test]
    fn suggest_terms_op_dispatches() {
        let params = serde_json::json!({
            "rows": [{"corpus_id": "a", "term_misses": ["merge"]}],
            "existing_terms": [],
            "min_count": 1
        });
        let response = suggest_terms_op(params).unwrap();
        assert_eq!(response.suggestions[0].term, "merge");
    }

    #[test]
    fn suggest_replacements_op_dispatches() {
        let params = serde_json::json!({
            "rows": [{
                "text": "Murch branchedes",
                "reference_text": "Merge branchen",
                "term_misses": ["merge"]
            }],
            "dictionary": {"terms": [], "replacements": {}},
            "min_confidence": 0.4
        });
        let response = suggest_replacements_op(params).unwrap();
        assert!(response
            .suggestions
            .iter()
            .any(|s| s.target.to_lowercase() == "merge"));
    }
}
