//! Golden-benchmark corpus loader + path resolution (Rust port of
//! `vp_benchmark.load_corpus` + `vp_benchmark_paths.resolve_corpus_manifest`,
//! Wave 6 follow-up to the dictionary-training CLI port).
//!
//! Two responsibilities:
//!
//! 1. Resolve where the `benchmark/corpus.json` manifest lives: explicit arg →
//!    `<app_root>/benchmark/corpus.json` → `<appdata>/benchmark/corpus.json`.
//!    [`resolve_corpus_manifest`] returns the first existing candidate (or the
//!    explicit path verbatim, even when missing, so the caller can include it in
//!    the error message). [`corpus_search_paths`] returns the candidate list
//!    sans the explicit arg, for the "no corpus found (looked: ...)" error.
//!
//! 2. Parse the manifest into a [`Vec<CorpusItem>`] with id, text, language,
//!    category, terms and audio path — the full shape the training CLI needs
//!    (the UI's existing `ui::corpus` parser only keeps id/text/language).
//!    Duplicate ids and missing/blank id/text fields are hard errors (matches
//!    `vp_benchmark._parse_item`).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::corpus_profile::HasLanguageAndCategory;

/// One fully-parsed corpus item. Mirrors `vp_benchmark.CorpusItem` field for
/// field (including the per-item `terms` curated list the training CLI feeds
/// into `extract_candidate_terms` as high-confidence candidates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusItem {
    pub id: String,
    pub text: String,
    pub audio: PathBuf,
    pub language: String,
    pub category: String,
    pub terms: Vec<String>,
}

impl HasLanguageAndCategory for CorpusItem {
    fn language(&self) -> &str {
        &self.language
    }
    fn category(&self) -> &str {
        &self.category
    }
}

/// Expand `~` and `$VAR` / `%VAR%` in a user-supplied path before any IO. Used
/// for `--benchmark-corpus` so an explicit `$WD_TEST_CORPUS_DIR/corpus.json`
/// resolves before the existence check.
pub fn expand_path(raw: &str) -> PathBuf {
    let expanded = expand_env_vars(raw);
    expand_tilde(&expanded)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix('~') {
        let home = home_dir();
        let rest = stripped.trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            return home;
        }
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Expand `$VAR` and `%VAR%` env-var references in `raw`. Unknown vars are
/// left untouched (mirrors Python's `os.path.expandvars` semantics, which
/// returns the substring verbatim when the variable is undefined). Handles
/// `$NAME`, `${NAME}` on POSIX and `%NAME%` on Windows-style paths.
fn expand_env_vars(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        match ch {
            '$' if i + 1 < bytes.len() && bytes[i + 1] as char == '{' => {
                let start = i + 2;
                if let Some(end) = raw[start..].find('}') {
                    let name = &raw[start..start + end];
                    match std::env::var(name) {
                        Ok(value) => out.push_str(&value),
                        Err(_) => out.push_str(&raw[i..start + end + 1]),
                    }
                    i = start + end + 1;
                    continue;
                }
                out.push(ch);
                i += 1;
            }
            '$' => {
                let start = i + 1;
                let end = raw[start..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .map(|n| start + n)
                    .unwrap_or(raw.len());
                if end == start {
                    out.push(ch);
                    i += 1;
                    continue;
                }
                let name = &raw[start..end];
                match std::env::var(name) {
                    Ok(value) => out.push_str(&value),
                    Err(_) => out.push_str(&raw[i..end]),
                }
                i = end;
            }
            '%' => {
                let start = i + 1;
                if let Some(end_rel) = raw[start..].find('%') {
                    let name = &raw[start..start + end_rel];
                    if !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        match std::env::var(name) {
                            Ok(value) => out.push_str(&value),
                            Err(_) => out.push_str(&raw[i..start + end_rel + 1]),
                        }
                        i = start + end_rel + 1;
                        continue;
                    }
                }
                out.push(ch);
                i += 1;
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// Resolve the golden-corpus manifest in priority order. Pure (no model load /
/// side effects). Priority: `explicit` (used verbatim if given, even when it
/// doesn't exist so the caller can report that exact path); else
/// `<app_root>/benchmark/corpus.json`; else `<appdata>/benchmark/corpus.json`.
/// Returns the first existing candidate (or the explicit path), else `None`.
pub fn resolve_corpus_manifest(
    app_root: Option<&Path>,
    explicit: Option<&Path>,
    appdata: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    for candidate in corpus_search_paths(app_root, appdata) {
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// The manifest locations [`resolve_corpus_manifest`] checks, for error display.
/// Mirrors the app-root + appdata candidates (sans the explicit arg, which only
/// exists when the user passes one) so the "no corpus found" message can list
/// exactly where the loader looked.
pub fn corpus_search_paths(app_root: Option<&Path>, appdata: Option<&Path>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(root) = app_root {
        paths.push(root.join("benchmark").join("corpus.json"));
    }
    if let Some(appdata) = appdata {
        paths.push(appdata.join("benchmark").join("corpus.json"));
    }
    paths
}

/// Load and parse a `benchmark/corpus.json` manifest. Hard error on:
/// missing/unreadable file, JSON parse failure, non-object root, missing/empty
/// `items` array, item with blank id/text, or a duplicate id (matches
/// `vp_benchmark.load_corpus`).
pub fn load_corpus(path: &Path) -> Result<Vec<CorpusItem>> {
    let raw = fs::read_to_string(path)?;
    let data: Value = serde_json::from_str(&raw)?;
    let object = data
        .as_object()
        .ok_or_else(|| anyhow!("corpus manifest root must be an object"))?;
    let audio_dir_raw = object
        .get("audio_dir")
        .and_then(Value::as_str)
        .unwrap_or("");
    let audio_dir = PathBuf::from(audio_dir_raw);
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();

    let items_val = object
        .get("items")
        .ok_or_else(|| anyhow!("corpus manifest must contain an items array"))?;
    let items_arr = items_val
        .as_array()
        .ok_or_else(|| anyhow!("corpus manifest must contain an items array"))?;

    let mut out: Vec<CorpusItem> = Vec::with_capacity(items_arr.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for raw_item in items_arr {
        let item = parse_item(raw_item, &audio_dir, &base, &mut seen)?;
        out.push(item);
    }
    Ok(out)
}

fn parse_item(
    raw: &Value,
    audio_dir: &Path,
    base: &Path,
    seen: &mut std::collections::HashSet<String>,
) -> Result<CorpusItem> {
    let obj = raw
        .as_object()
        .ok_or_else(|| anyhow!("corpus item must be an object"))?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let text = obj
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    if id.is_empty() || text.is_empty() {
        return Err(anyhow!("corpus item requires id and text"));
    }
    if !seen.insert(id.clone()) {
        return Err(anyhow!("duplicate corpus id: {id}"));
    }
    let language = obj
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let category = obj
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let audio = parse_audio(obj, &id, audio_dir, base);
    let terms = parse_terms(obj, &id)?;
    Ok(CorpusItem {
        id,
        text,
        audio,
        language,
        category,
        terms,
    })
}

fn parse_audio(
    obj: &serde_json::Map<String, Value>,
    item_id: &str,
    audio_dir: &Path,
    base: &Path,
) -> PathBuf {
    let raw = obj
        .get("audio")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            audio_dir
                .join(format!("{item_id}.wav"))
                .to_string_lossy()
                .into_owned()
        });
    let audio = PathBuf::from(raw);
    if audio.is_absolute() {
        audio
    } else {
        base.join(audio)
    }
}

fn parse_terms(obj: &serde_json::Map<String, Value>, item_id: &str) -> Result<Vec<String>> {
    match obj.get("terms") {
        Some(Value::Array(arr)) => Ok(arr
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
            .collect()),
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(anyhow!("corpus item {item_id}: terms must be an array")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_manifest(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("corpus.json");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_well_formed_corpus_with_terms_and_categories() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"{
              "version": 1,
              "audio_dir": "audio",
              "items": [
                {"id":"a","language":"da","category":"short_danish","text":"Hej","terms":["Hej"]},
                {"id":"b","language":"en","category":"product_names","text":"Codex","terms":["Codex","Claude Code"]}
              ]
            }"#,
        );
        let items = load_corpus(&path).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "a");
        assert_eq!(items[1].terms, vec!["Codex", "Claude Code"]);
    }

    #[test]
    fn rejects_duplicate_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"{"items":[{"id":"a","text":"x"},{"id":"a","text":"y"}]}"#,
        );
        let err = load_corpus(&path).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_blank_id_or_text() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(tmp.path(), r#"{"items":[{"id":"","text":"x"}]}"#);
        assert!(load_corpus(&path).is_err());
        let path = write_manifest(tmp.path(), r#"{"items":[{"id":"a","text":""}]}"#);
        assert!(load_corpus(&path).is_err());
    }

    #[test]
    fn rejects_missing_items_array() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(tmp.path(), r#"{"version":1}"#);
        assert!(load_corpus(&path).is_err());
    }

    #[test]
    fn rejects_non_array_terms() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"{"items":[{"id":"a","text":"x","terms":"oops"}]}"#,
        );
        let err = load_corpus(&path).unwrap_err();
        assert!(err.to_string().contains("terms must be an array"));
    }

    #[test]
    fn audio_defaults_to_audio_dir_slash_id_wav() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"{"audio_dir":"audio","items":[{"id":"a","text":"x"}]}"#,
        );
        let items = load_corpus(&path).unwrap();
        assert!(items[0].audio.ends_with(Path::new("audio").join("a.wav")));
    }

    #[test]
    fn explicit_audio_path_kept_verbatim_when_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let absolute = if cfg!(windows) {
            "C:\\\\sounds\\\\x.wav"
        } else {
            "/tmp/sounds/x.wav"
        };
        let body = format!(
            r#"{{"items":[{{"id":"a","text":"x","audio":"{}"}}]}}"#,
            absolute
        );
        let path = write_manifest(tmp.path(), &body);
        let items = load_corpus(&path).unwrap();
        assert!(items[0].audio.is_absolute());
    }

    #[test]
    fn resolve_returns_explicit_verbatim_even_when_missing() {
        let explicit = PathBuf::from("/nonexistent/corpus.json");
        let resolved = resolve_corpus_manifest(None, Some(&explicit), None);
        assert_eq!(resolved, Some(explicit));
    }

    #[test]
    fn resolve_picks_app_root_then_appdata() {
        let tmp = tempfile::tempdir().unwrap();
        let app_root = tmp.path().join("app");
        let appdata = tmp.path().join("appdata");
        fs::create_dir_all(app_root.join("benchmark")).unwrap();
        fs::create_dir_all(appdata.join("benchmark")).unwrap();
        // Only appdata has the file initially.
        fs::write(appdata.join("benchmark").join("corpus.json"), "{}").unwrap();
        let resolved = resolve_corpus_manifest(Some(&app_root), None, Some(&appdata));
        assert_eq!(
            resolved.unwrap(),
            appdata.join("benchmark").join("corpus.json")
        );
        // Now also create one at app_root, which wins.
        fs::write(app_root.join("benchmark").join("corpus.json"), "{}").unwrap();
        let resolved = resolve_corpus_manifest(Some(&app_root), None, Some(&appdata));
        assert_eq!(
            resolved.unwrap(),
            app_root.join("benchmark").join("corpus.json")
        );
    }

    #[test]
    fn search_paths_lists_both_when_provided() {
        let app_root = PathBuf::from("/app");
        let appdata = PathBuf::from("/appdata");
        let paths = corpus_search_paths(Some(&app_root), Some(&appdata));
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with(Path::new("benchmark").join("corpus.json")));
    }

    #[test]
    fn expand_path_handles_tilde() {
        let p = expand_path("~/corpus.json");
        assert!(!p.to_string_lossy().contains('~'));
    }

    #[test]
    fn expand_path_handles_env_var_dollar() {
        // Use a name we set ourselves; std::env::set_var is process-wide so
        // pick something unlikely to clash with other tests.
        let name = "WD_TRAIN_CLI_TEST_BASE_DOLLAR";
        let value = std::env::temp_dir().join("vp_train_corp");
        std::env::set_var(name, &value);
        let p = expand_path(&format!("${name}/corpus.json"));
        std::env::remove_var(name);
        assert!(p
            .to_string_lossy()
            .contains(value.to_string_lossy().as_ref()));
    }

    #[test]
    fn expand_path_handles_env_var_percent() {
        let name = "WD_TRAIN_CLI_TEST_BASE_PERCENT";
        let value = std::env::temp_dir().join("vp_train_corp_pct");
        std::env::set_var(name, &value);
        let p = expand_path(&format!("%{name}%/corpus.json"));
        std::env::remove_var(name);
        assert!(p
            .to_string_lossy()
            .contains(value.to_string_lossy().as_ref()));
    }

    #[test]
    fn expand_path_keeps_unknown_var_verbatim() {
        let raw = "$WD_TRAIN_DEFINITELY_UNSET_VAR/x.json";
        let p = expand_path(raw);
        assert!(p
            .to_string_lossy()
            .contains("WD_TRAIN_DEFINITELY_UNSET_VAR"));
    }
}
