//! Append candidate terms to an existing dictionary, deduping
//! case-insensitively. Pure / no IO — returns a [`MergePreview`] the CLI
//! either prints (preview mode) or hands to the store layer to write.

use std::collections::HashSet;

use super::{normalize, MergePreview};

/// Append candidate terms to `existing`, deduping case-insensitively. Mirrors
/// the Python `merge_terms` — `candidates` may be plain strings; existing
/// terms are preserved in order; the `skipped_existing` list is deduped by
/// normalised form so "Kubectl" + "kubectl" against an existing "kubectl"
/// register only once as a skip.
pub fn merge_terms(existing: &[String], candidates: &[String]) -> MergePreview {
    let existing_terms: Vec<String> = existing
        .iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect();
    let mut seen: HashSet<String> = existing_terms.iter().map(|t| normalize(t)).collect();
    let mut skipped_seen: HashSet<String> = HashSet::new();
    let mut preview = MergePreview {
        result_terms: existing_terms.clone(),
        existing_count: existing_terms.len(),
        ..MergePreview::default()
    };
    for candidate in candidates {
        let term = candidate.trim();
        let norm = normalize(term);
        if norm.is_empty() {
            continue;
        }
        if seen.contains(&norm) {
            if skipped_seen.insert(norm) {
                preview.skipped_existing.push(term.to_owned());
            }
            continue;
        }
        seen.insert(norm);
        preview.added.push(term.to_owned());
        preview.result_terms.push(term.to_owned());
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_appends_new_terms() {
        let preview = merge_terms(
            &["Existing".to_owned()],
            &["New".to_owned(), "Another".to_owned()],
        );
        assert_eq!(preview.added, vec!["New", "Another"]);
        assert_eq!(preview.result_terms, vec!["Existing", "New", "Another"]);
        assert_eq!(preview.existing_count, 1);
    }

    #[test]
    fn merge_dedup_case_insensitive_against_existing() {
        let preview = merge_terms(
            &["Claude Code".to_owned()],
            &["claude code".to_owned(), "Codex".to_owned()],
        );
        assert_eq!(preview.added, vec!["Codex"]);
        assert!(preview.skipped_existing.contains(&"claude code".to_owned()));
    }

    #[test]
    fn merge_skipped_existing_deduped_case_insensitively() {
        let preview = merge_terms(
            &["kubectl".to_owned()],
            &[
                "Kubectl".to_owned(),
                "kubectl".to_owned(),
                "KUBECTL".to_owned(),
            ],
        );
        assert!(preview.added.is_empty());
        assert_eq!(preview.skipped_existing.len(), 1);
    }

    #[test]
    fn merge_dedup_within_candidates() {
        let preview = merge_terms(&[], &["MCP".to_owned(), "mcp".to_owned(), "RAG".to_owned()]);
        assert_eq!(preview.added, vec!["MCP", "RAG"]);
    }

    #[test]
    fn merge_blank_candidates_ignored() {
        let preview = merge_terms(&[], &["  ".to_owned(), "".to_owned(), "Real".to_owned()]);
        assert_eq!(preview.added, vec!["Real"]);
    }
}
