//! CLI orchestration for corpus → dictionary training (Rust port of
//! `vp_dictionary_training_cli.py`, Wave 6 follow-up to #348).
//!
//! Glues the PURE term-mining logic (`extract`, `merge`, `misses` in this
//! module's siblings) to the IO (corpus loader in [`crate::corpus`], dictionary
//! store in [`crate::dictionary::store`]) and surfaces two CLI entry points:
//!
//! * [`run_build_from_corpus`] (`dictionary build-from-corpus`) — extract
//!   candidate terms from the (optionally profile-filtered) corpus reference
//!   TEXT and APPEND+DEDUP them into the dictionary. Defaults to a PREVIEW;
//!   only writes when `apply: true` (`--apply`). It NEVER records or reads
//!   corpus audio.
//! * [`run_suggest_from_misses`] (`dictionary suggest-terms`) — read an
//!   annotated benchmark JSONL and surface the domain terms the model missed
//!   as SUGGESTED dictionary additions (preview), writing only on
//!   `apply: true`.
//!
//! Both print a clear preview and emit `--json` for tooling. The
//! preview-by-default safety net mirrors the Python CLI bit-for-bit so an
//! automation flipping from the Python flag to the Rust subcommand sees the
//! same gates and the same JSON schema.
//!
//! Reporting (preview text + JSON envelopes + the shared `fail` helper) is in
//! the sibling [`super::cli_report`] module to stay under the AGENTS.md per-
//! file LOC cap.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value};

use super::cli_report::{fail, report_build, report_suggestions, BuildReport, SuggestReport};
use super::BenchmarkRow;
use super::{extract_candidate_terms, merge_terms, suggest_terms_from_misses};
use crate::corpus::{
    corpus_search_paths, expand_path, load_corpus, resolve_corpus_manifest, CorpusItem,
};
use crate::corpus_profile::{build_profile, filter_corpus_items, CorpusProfile};
use crate::dictionary::store::{
    load_dictionary_document, resolve_dictionary_path, terms_from_document, write_terms,
};

/// Knobs forwarded from clap into [`run_build_from_corpus`].
#[derive(Debug, Clone, Default)]
pub struct BuildFromCorpusOptions {
    pub corpus_manifest: Option<String>,
    pub app_root: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub dictionary_path: Option<String>,
    pub language: Option<String>,
    pub category: Option<String>,
    pub min_count: usize,
    pub apply: bool,
    pub as_json: bool,
}

/// Knobs forwarded from clap into [`run_suggest_from_misses`].
#[derive(Debug, Clone)]
pub struct SuggestFromMissesOptions {
    pub jsonl_path: PathBuf,
    pub dictionary_path: Option<String>,
    pub min_count: usize,
    pub apply: bool,
    pub as_json: bool,
}

/// `dictionary build-from-corpus`: grow the dictionary from corpus reference
/// TEXT. Loads the corpus, applies the language/category profile, extracts
/// candidates, merges them (append + case-insensitive dedup) against the
/// existing dictionary, and PREVIEWS what would be added. Writes only when
/// `opts.apply` is true. Returns a process exit code (0 on success, 1 on a
/// corpus/dictionary error). Never records or reads audio.
pub fn run_build_from_corpus(opts: BuildFromCorpusOptions) -> i32 {
    let profile = build_profile(opts.language.as_deref(), opts.category.as_deref());

    let items = match load_filtered_corpus(&opts, &profile) {
        Ok(items) => items,
        Err(err) => return fail(&err.to_string(), opts.as_json),
    };

    // A SPECIFIED profile that matches zero corpus items is almost always a
    // mistyped --language / --category: the build would otherwise proceed
    // silently and write nothing with no signal. Fail clearly (#272). An empty
    // profile (no filter) is left alone — out of scope here.
    if items.is_empty() && !profile.is_empty() {
        return fail(
            &format!("no corpus items matched profile {}", profile.describe()),
            opts.as_json,
        );
    }

    let (dict_path, document, existing) = match prepare_dictionary(opts.dictionary_path.as_deref())
    {
        Ok(triple) => triple,
        Err(err) => return fail(&err.to_string(), opts.as_json),
    };

    let texts: Vec<String> = items.iter().map(|item| item.text.clone()).collect();
    let item_terms: Vec<Vec<String>> = items.iter().map(|item| item.terms.clone()).collect();
    let item_ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
    let candidates =
        extract_candidate_terms(&texts, Some(&item_terms), Some(&item_ids), opts.min_count);
    let candidate_terms: Vec<String> = candidates.iter().map(|c| c.term.clone()).collect();
    let preview = merge_terms(&existing, &candidate_terms);

    let mut wrote = false;
    if opts.apply && !preview.added.is_empty() {
        if let Err(err) = write_terms(&dict_path, &preview.result_terms, Some(document)) {
            return fail(&err.to_string(), opts.as_json);
        }
        wrote = true;
    }

    report_build(&BuildReport {
        preview: &preview,
        candidates: &candidates,
        profile: &profile,
        items: &items,
        dict_path: &dict_path,
        wrote,
        as_json: opts.as_json,
    });
    0
}

/// `dictionary suggest-terms`: suggest dictionary terms from benchmark misses.
/// Reads an annotated JSONL, surfaces the domain terms the model missed
/// (`term_misses`) as SUGGESTED additions flagged against the current
/// dictionary, and PREVIEWS them. Writes not-yet-present suggestions only
/// when `opts.apply` is true. Returns a process exit code.
pub fn run_suggest_from_misses(opts: SuggestFromMissesOptions) -> i32 {
    let (dict_path, document, existing) = match prepare_dictionary(opts.dictionary_path.as_deref())
    {
        Ok(triple) => triple,
        Err(err) => return fail(&err.to_string(), opts.as_json),
    };

    let rows = match read_jsonl(&opts.jsonl_path) {
        Ok(rows) => rows,
        Err(err) => return fail(&err.to_string(), opts.as_json),
    };

    let suggestions = suggest_terms_from_misses(&rows, &existing, opts.min_count);
    let new_terms: Vec<String> = suggestions
        .iter()
        .filter(|s| !s.already_in_dictionary)
        .map(|s| s.term.clone())
        .collect();

    let mut wrote = false;
    if opts.apply && !new_terms.is_empty() {
        let preview = merge_terms(&existing, &new_terms);
        if let Err(err) = write_terms(&dict_path, &preview.result_terms, Some(document)) {
            return fail(&err.to_string(), opts.as_json);
        }
        wrote = true;
    }

    report_suggestions(&SuggestReport {
        suggestions: &suggestions,
        new_terms: &new_terms,
        dict_path: &dict_path,
        wrote,
        as_json: opts.as_json,
    });
    0
}

/// Resolve the corpus manifest, load it, and apply the profile filter.
/// Surfaces a single user-facing error string for any failure (missing
/// manifest, parse failure, etc.) so the caller can `_fail` it.
fn load_filtered_corpus(
    opts: &BuildFromCorpusOptions,
    profile: &CorpusProfile,
) -> Result<Vec<CorpusItem>> {
    let app_root_owned: Option<PathBuf> =
        opts.app_root.clone().or_else(|| Some(PathBuf::from(".")));
    let appdata_owned = opts.appdata.clone();
    let explicit_owned: Option<PathBuf> = opts
        .corpus_manifest
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    let manifest = resolve_corpus_manifest(
        app_root_owned.as_deref(),
        explicit_owned.as_deref(),
        appdata_owned.as_deref(),
    );
    // Expand ~/$VARS so an explicit "--benchmark-corpus ~/corpus.json" works
    // (the resolver returned the explicit path verbatim) and the existence
    // check matches what `load_corpus` will actually open.
    let expanded: Option<PathBuf> = manifest
        .as_ref()
        .map(|p| expand_path(p.to_string_lossy().as_ref()));
    if expanded.as_ref().is_none_or(|p| !p.exists()) {
        let mut looked: Vec<String> =
            corpus_search_paths(app_root_owned.as_deref(), appdata_owned.as_deref())
                .into_iter()
                .map(|p| p.display().to_string())
                .collect();
        if let Some(explicit) = expanded.as_ref() {
            looked.insert(0, explicit.display().to_string());
        }
        return Err(anyhow::anyhow!(
            "no benchmark corpus found (looked: {})",
            looked.join(", ")
        ));
    }
    let resolved = expanded.expect("checked Some above");
    let items = load_corpus(&resolved)?;
    Ok(filter_corpus_items(&items, profile))
}

fn prepare_dictionary(path: Option<&str>) -> Result<(PathBuf, Map<String, Value>, Vec<String>)> {
    let dict_path = resolve_dictionary_path(path)?;
    let document = load_dictionary_document(&dict_path)?;
    let existing = terms_from_document(&document);
    Ok((dict_path, document, existing))
}

/// Read newline-delimited JSON objects from `path`, skipping blanks /
/// unparseable lines. Mirrors Python's tolerance for partial benchmark
/// captures (a half-written row at the end of a file does not abort the run).
fn read_jsonl(path: &Path) -> std::io::Result<Vec<BenchmarkRow>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out: Vec<BenchmarkRow> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<BenchmarkRow>(trimmed) {
            out.push(row);
        }
        // Silently skip unparseable lines (matches the Python loader's
        // `try/except JSONDecodeError: continue`).
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_corpus(dir: &Path) -> PathBuf {
        let path = dir.join("corpus.json");
        fs::write(
            &path,
            r#"{
              "version": 1,
              "audio_dir": "audio",
              "items": [
                {"id":"da-tech-001","language":"da","category":"mixed_technical",
                 "text":"Skift backend til Parakeet og behold dictionary replacements.",
                 "terms":["Parakeet","dictionary","replacements"]},
                {"id":"en-short-001","language":"en","category":"short_english",
                 "text":"Please check the latest build.","terms":[]},
                {"id":"da-prod-001","language":"da","category":"product_names",
                 "text":"Claude Code og Codex skal forstå prompten.",
                 "terms":["Claude Code","Codex"]}
              ]
            }"#,
        )
        .unwrap();
        path
    }

    fn build_opts(manifest: &Path, dict_path: &Path) -> BuildFromCorpusOptions {
        BuildFromCorpusOptions {
            corpus_manifest: Some(manifest.to_string_lossy().into_owned()),
            app_root: None,
            appdata: None,
            dictionary_path: Some(dict_path.to_string_lossy().into_owned()),
            language: None,
            category: None,
            min_count: 1,
            apply: false,
            as_json: false,
        }
    }

    #[test]
    fn build_preview_does_not_write() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_corpus(tmp.path());
        let dict = tmp.path().join("dictionary.json");
        let rc = run_build_from_corpus(build_opts(&manifest, &dict));
        assert_eq!(rc, 0);
        assert!(!dict.exists());
    }

    #[test]
    fn build_apply_writes_terms_to_dictionary() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_corpus(tmp.path());
        let dict = tmp.path().join("dictionary.json");
        let mut opts = build_opts(&manifest, &dict);
        opts.apply = true;
        let rc = run_build_from_corpus(opts);
        assert_eq!(rc, 0);
        assert!(dict.exists());
        let raw = fs::read_to_string(&dict).unwrap();
        assert!(raw.contains("Parakeet"));
        assert!(raw.contains("Claude Code"));
    }

    #[test]
    fn build_profile_filter_restricts_to_subset() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_corpus(tmp.path());
        let dict = tmp.path().join("dictionary.json");
        // Run with profile filter to product_names only — should pick "Claude
        // Code" + "Codex" but NOT "Parakeet" (which is in a different
        // category).
        let mut opts = build_opts(&manifest, &dict);
        opts.language = Some("da".to_owned());
        opts.category = Some("product_names".to_owned());
        opts.apply = true;
        let rc = run_build_from_corpus(opts);
        assert_eq!(rc, 0);
        let raw = fs::read_to_string(&dict).unwrap();
        assert!(raw.contains("Claude Code"));
        assert!(!raw.contains("Parakeet"));
    }

    #[test]
    fn build_zero_match_profile_returns_one_with_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_corpus(tmp.path());
        let dict = tmp.path().join("dictionary.json");
        let mut opts = build_opts(&manifest, &dict);
        opts.language = Some("fr".to_owned()); // no French items
        let rc = run_build_from_corpus(opts);
        assert_eq!(rc, 1);
        assert!(!dict.exists());
    }

    #[test]
    fn build_missing_corpus_returns_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dict = tmp.path().join("dictionary.json");
        let mut opts = build_opts(&tmp.path().join("nope.json"), &dict);
        opts.as_json = true;
        let rc = run_build_from_corpus(opts);
        assert_eq!(rc, 1);
    }

    #[test]
    fn build_invalid_corpus_json_returns_one() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("bad.json");
        fs::write(&manifest, "{not json").unwrap();
        let dict = tmp.path().join("dictionary.json");
        let mut opts = build_opts(&manifest, &dict);
        opts.as_json = true;
        let rc = run_build_from_corpus(opts);
        assert_eq!(rc, 1);
    }

    #[test]
    fn build_empty_dictionary_path_returns_one() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_corpus(tmp.path());
        let mut opts = build_opts(&manifest, Path::new(""));
        opts.dictionary_path = Some(String::new());
        opts.as_json = true;
        let rc = run_build_from_corpus(opts);
        assert_eq!(rc, 1);
    }

    fn write_jsonl(dir: &Path) -> PathBuf {
        let path = dir.join("results.jsonl");
        let body = r#"{"corpus_id": "da-tech-004", "term_misses": ["merge", "deploy"]}
{"corpus_id": "en-tech-002", "term_misses": ["NVIDIA Parakeet"]}

not-json
"#;
        fs::write(&path, body).unwrap();
        path
    }

    fn suggest_opts(jsonl: &Path, dict_path: &Path) -> SuggestFromMissesOptions {
        SuggestFromMissesOptions {
            jsonl_path: jsonl.to_path_buf(),
            dictionary_path: Some(dict_path.to_string_lossy().into_owned()),
            min_count: 1,
            apply: false,
            as_json: false,
        }
    }

    #[test]
    fn suggest_preview_lists_new_and_existing_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let dict = tmp.path().join("dictionary.json");
        fs::write(&dict, r#"{"terms":["deploy"],"replacements":{}}"#).unwrap();
        let jsonl = write_jsonl(tmp.path());
        let mut opts = suggest_opts(&jsonl, &dict);
        opts.as_json = true;
        let rc = run_suggest_from_misses(opts);
        assert_eq!(rc, 0);
        // dict file unchanged on preview
        let raw = fs::read_to_string(&dict).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["terms"], serde_json::json!(["deploy"]));
    }

    #[test]
    fn suggest_apply_adds_only_new_terms() {
        let tmp = tempfile::tempdir().unwrap();
        let dict = tmp.path().join("dictionary.json");
        fs::write(&dict, r#"{"terms":["deploy"],"replacements":{}}"#).unwrap();
        let jsonl = write_jsonl(tmp.path());
        let mut opts = suggest_opts(&jsonl, &dict);
        opts.apply = true;
        let rc = run_suggest_from_misses(opts);
        assert_eq!(rc, 0);
        let raw = fs::read_to_string(&dict).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        let terms = parsed["terms"].as_array().unwrap();
        let count_deploy = terms
            .iter()
            .filter(|t| t.as_str() == Some("deploy"))
            .count();
        assert_eq!(count_deploy, 1, "deploy should not be duplicated");
        assert!(terms.iter().any(|t| t.as_str() == Some("merge")));
        assert!(terms.iter().any(|t| t.as_str() == Some("NVIDIA Parakeet")));
    }

    #[test]
    fn suggest_missing_jsonl_returns_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dict = tmp.path().join("dictionary.json");
        fs::write(&dict, r#"{"terms":[],"replacements":{}}"#).unwrap();
        let opts = suggest_opts(&tmp.path().join("missing.jsonl"), &dict);
        let rc = run_suggest_from_misses(opts);
        assert_eq!(rc, 1);
    }
}
