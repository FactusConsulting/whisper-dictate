//! CLI adapter for `whisper-dictate dictionary suggest-replacements`.
//!
//! Thin wrapper around the pure `suggest_replacements_from_rows` helper:
//! resolves the dictionary path, reads the JSONL snapshot, and prints the
//! result as a preview or single-line JSON array. Preview-only — the caller
//! promotes accepted suggestions with `dictionary replace FROM=TO`. Audit
//! item 4 (`docs/architecture-audit-2026-07-16.md`): this replaces the
//! Python `--dictionary-suggest` in-process code path.
//!
//! Kept in a sibling file so `suggest/mod.rs` stays under the ~500 LOC gate.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::{
    suggest_replacements_from_rows, DictionarySnapshot, ReplacementSuggestion, SuggestRow,
};
use crate::dictionary::store::{load_dictionary, resolve_dictionary_path};

/// Knobs forwarded from clap into [`run_suggest_replacements`].
#[derive(Debug, Clone)]
pub struct SuggestReplacementsOptions {
    /// Path to the JSONL to scan.
    pub jsonl_path: String,
    /// Optional `--dictionary` override; falls back to `$VOICEPI_DICTIONARY`
    /// then the per-user default.
    pub dictionary_path: Option<String>,
    /// Minimum fuzzy confidence to surface (default 0.62).
    pub min_confidence: f64,
    /// Emit machine-readable JSON.
    pub as_json: bool,
}

/// `dictionary suggest-replacements`: preview fuzzy replacement suggestions
/// mined from a benchmark / history JSONL. Returns a process exit code:
/// `0` on success (empty or non-empty preview), `1` on unrecoverable input
/// errors (missing file, malformed dictionary). Never writes to disk.
pub fn run_suggest_replacements(opts: SuggestReplacementsOptions) -> i32 {
    let snapshot = match load_snapshot(opts.dictionary_path.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(err) => return fail(&err.to_string(), opts.as_json),
    };
    let rows = match read_jsonl(Path::new(&opts.jsonl_path)) {
        Ok(rows) => rows,
        Err(err) => return fail(&err.to_string(), opts.as_json),
    };
    let suggestions = suggest_replacements_from_rows(&rows, &snapshot, opts.min_confidence);
    print_suggestions(&suggestions, opts.as_json);
    0
}

fn load_snapshot(path: Option<&str>) -> Result<DictionarySnapshot> {
    let resolved = resolve_dictionary_path(path)?;
    // A missing dictionary is a first-run condition, not an error — an
    // empty snapshot still yields useful "did the text look wrong?" fuzzy
    // suggestions against the reference text alone.
    if !resolved.exists() {
        return Ok(DictionarySnapshot::default());
    }
    let dictionary = load_dictionary(&resolved)?;
    let replacements: BTreeMap<String, String> = dictionary
        .replacements
        .into_iter()
        .map(|r| (r.from, r.to))
        .collect();
    Ok(DictionarySnapshot {
        terms: dictionary.terms,
        replacements,
    })
}

/// Read newline-delimited JSON `SuggestRow` records; silently skip blank /
/// unparseable lines to match the Python loader's tolerance for partial
/// benchmark captures (a half-written trailing row must not abort the run).
fn read_jsonl(path: &Path) -> std::io::Result<Vec<SuggestRow>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out: Vec<SuggestRow> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<SuggestRow>(trimmed) {
            out.push(row);
        }
    }
    Ok(out)
}

fn print_suggestions(suggestions: &[ReplacementSuggestion], as_json: bool) {
    if as_json {
        // Emit each suggestion in the `{from, to, count, confidence, reason,
        // samples}` shape the pre-Wave-8 Python printer used — tooling that
        // parses `--dictionary-suggest --json` keeps working.
        let payload: Vec<Value> = suggestions
            .iter()
            .map(|s| {
                let samples: Vec<&str> = s.samples.iter().take(5).map(String::as_str).collect();
                serde_json::json!({
                    "from": s.source,
                    "to": s.target,
                    "count": s.count,
                    "confidence": round3(s.confidence),
                    "reason": s.reason,
                    "samples": samples,
                })
            })
            .collect();
        if let Ok(s) = serde_json::to_string(&payload) {
            println!("{s}");
        }
        return;
    }
    if suggestions.is_empty() {
        println!("No dictionary replacement suggestions found.");
        return;
    }
    for s in suggestions {
        let samples = if s.samples.is_empty() {
            String::new()
        } else {
            let joined = s
                .samples
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            format!(" samples={joined}")
        };
        println!(
            "{:?} -> {:?}  count={} confidence={:.2} reason={}{}",
            s.source, s.target, s.count, s.confidence, s.reason, samples
        );
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn fail(message: &str, as_json: bool) -> i32 {
    if as_json {
        if let Ok(s) = serde_json::to_string(&serde_json::json!({"error": message})) {
            println!("{s}");
        }
    } else {
        println!("error: {message}");
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(dir: &Path, lines: &[&str]) -> std::path::PathBuf {
        let path = dir.join("rows.jsonl");
        let mut f = File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    fn write_dictionary(dir: &Path, terms: &[&str]) -> std::path::PathBuf {
        let path = dir.join("dictionary.json");
        let doc = serde_json::json!({
            "terms": terms,
            "replacements": {},
        });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        path
    }

    #[test]
    fn suggest_replacements_json_output_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl = write_jsonl(
            tmp.path(),
            &[
                "",
                r#"{"corpus_id":"a","text":"Clort kode should work","term_misses":["Claude Code"]}"#,
            ],
        );
        let dict = write_dictionary(tmp.path(), &["Claude Code"]);
        let rc = run_suggest_replacements(SuggestReplacementsOptions {
            jsonl_path: jsonl.to_string_lossy().into_owned(),
            dictionary_path: Some(dict.to_string_lossy().into_owned()),
            min_confidence: 0.35,
            as_json: true,
        });
        assert_eq!(rc, 0);
    }

    #[test]
    fn suggest_replacements_missing_dictionary_still_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl = write_jsonl(
            tmp.path(),
            &[r#"{"text":"anything","reference_text":"anything"}"#],
        );
        // Point at a nonexistent dictionary — first-run condition, must not
        // fail (empty snapshot).
        let dict = tmp.path().join("missing.json");
        let rc = run_suggest_replacements(SuggestReplacementsOptions {
            jsonl_path: jsonl.to_string_lossy().into_owned(),
            dictionary_path: Some(dict.to_string_lossy().into_owned()),
            min_confidence: 0.62,
            as_json: false,
        });
        assert_eq!(rc, 0);
    }

    #[test]
    fn suggest_replacements_missing_jsonl_returns_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dict = write_dictionary(tmp.path(), &[]);
        let rc = run_suggest_replacements(SuggestReplacementsOptions {
            jsonl_path: tmp
                .path()
                .join("missing.jsonl")
                .to_string_lossy()
                .into_owned(),
            dictionary_path: Some(dict.to_string_lossy().into_owned()),
            min_confidence: 0.62,
            as_json: true,
        });
        assert_eq!(rc, 1);
    }
}
