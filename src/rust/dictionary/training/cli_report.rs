//! Preview / JSON reporting for the `build-from-corpus` and `suggest-terms`
//! subcommands. Split off the orchestration entry points (`cli.rs`) to keep
//! every file under the ~500 LOC modularity cap (AGENTS.md). The JSON shape
//! emitted here is a stable contract: it mirrors what
//! `vp_dictionary_training_cli.py` printed pre-Wave 6 so any tooling parsing
//! the stdout payload keeps working when the dispatcher flips over.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::{MergePreview, TermCandidate, TermSuggestion};
use crate::corpus::CorpusItem;
use crate::corpus_profile::CorpusProfile;

/// Inputs for [`report_build`] — bundled in a struct so callers don't end up
/// with a 7-arg function signature (clippy lint + readability).
pub(super) struct BuildReport<'a> {
    pub preview: &'a MergePreview,
    pub candidates: &'a [TermCandidate],
    pub profile: &'a CorpusProfile,
    pub items: &'a [CorpusItem],
    pub dict_path: &'a Path,
    pub wrote: bool,
    pub as_json: bool,
}

/// Inputs for [`report_suggestions`].
pub(super) struct SuggestReport<'a> {
    pub suggestions: &'a [TermSuggestion],
    pub new_terms: &'a [String],
    pub dict_path: &'a Path,
    pub wrote: bool,
    pub as_json: bool,
}

/// Print the `build-from-corpus` preview (or JSON payload).
pub(super) fn report_build(report: &BuildReport<'_>) {
    if report.as_json {
        let candidates: Vec<Value> = report
            .candidates
            .iter()
            .map(|c| {
                serde_json::json!({
                    "term": c.term,
                    "count": c.count,
                    "reason": c.reason,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "command": "build-from-corpus",
            "dictionary": report.dict_path.display().to_string(),
            "profile": report.profile.describe(),
            "corpus_items": report.items.len(),
            "existing_terms": report.preview.existing_count,
            "added": report.preview.added,
            "skipped_existing": report.preview.skipped_existing,
            "candidates": candidates,
            "applied": report.wrote,
        });
        emit_json(&payload);
        return;
    }
    println!(
        "[dictionary] build-from-corpus reads corpus reference TEXT only \
         (never records audio)."
    );
    println!(
        "  corpus selection: {}  ({} item(s))",
        report.profile.describe(),
        report.items.len()
    );
    println!(
        "  dictionary: {}  ({} existing term(s))",
        report.dict_path.display(),
        report.preview.existing_count
    );
    if !report.preview.added.is_empty() {
        println!("  would add {} new term(s):", report.preview.added.len());
        for term in &report.preview.added {
            println!("    + {term}");
        }
    } else {
        println!("  no new terms to add (all candidates already present).");
    }
    if !report.preview.skipped_existing.is_empty() {
        println!(
            "  skipped {} already present.",
            report.preview.skipped_existing.len()
        );
    }
    if report.wrote {
        println!(
            "  WROTE {} term(s) to {}",
            report.preview.added.len(),
            report.dict_path.display()
        );
    } else if !report.preview.added.is_empty() {
        println!("  PREVIEW only — re-run with --apply to write these terms.");
    }
}

/// Print the `suggest-terms` preview (or JSON payload).
pub(super) fn report_suggestions(report: &SuggestReport<'_>) {
    if report.as_json {
        let suggestions: Vec<Value> = report
            .suggestions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "term": s.term,
                    "count": s.count,
                    "samples": s.samples,
                    "already_in_dictionary": s.already_in_dictionary,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "command": "suggest-from-benchmark-misses",
            "dictionary": report.dict_path.display().to_string(),
            "suggestions": suggestions,
            "new_terms": report.new_terms,
            "applied": report.wrote,
        });
        emit_json(&payload);
        return;
    }
    println!(
        "[dictionary] suggest-from-benchmark-misses reads benchmark result TEXT \
         only (never records audio)."
    );
    println!("  dictionary: {}", report.dict_path.display());
    if report.suggestions.is_empty() {
        println!("  no missed domain terms found in the benchmark results.");
        return;
    }
    println!(
        "  {} suggested term(s) from benchmark misses:",
        report.suggestions.len()
    );
    for s in report.suggestions {
        let mark = if s.already_in_dictionary {
            "(already in dictionary)"
        } else {
            "NEW"
        };
        let sample_preview = s
            .samples
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let samples_str = if sample_preview.is_empty() {
            String::new()
        } else {
            format!("  samples={sample_preview}")
        };
        println!(
            "    {:>22}  {:?}  count={}{}",
            mark, s.term, s.count, samples_str
        );
    }
    if report.wrote {
        println!(
            "  WROTE {} new term(s) to {}",
            report.new_terms.len(),
            report.dict_path.display()
        );
    } else if !report.new_terms.is_empty() {
        println!(
            "  PREVIEW only — re-run with --apply to add the {} NEW term(s).",
            report.new_terms.len()
        );
    }
}

pub(super) fn emit_json(value: &impl Serialize) {
    if let Ok(s) = serde_json::to_string(value) {
        println!("{s}");
    }
}

/// Surface a one-line user error (text or JSON envelope) and return exit
/// code 1. Shared by both subcommands so the failure shape stays consistent.
pub(super) fn fail(message: &str, as_json: bool) -> i32 {
    if as_json {
        let payload = serde_json::json!({"error": message});
        emit_json(&payload);
    } else {
        println!("error: {message}");
    }
    1
}
