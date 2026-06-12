//! Parsing + path resolution for the golden-benchmark corpus, used by the
//! System tab's "record a corpus item from the app" picker.
//!
//! The worker ships `benchmark/corpus.json` at the app root (since #234) and
//! resolves recordings in the per-user appdata dir. The Rust UI reads the SAME
//! manifest so its picker can list the corpus items (id + a short text preview)
//! and mark which ones already have a recording in appdata — then launch the
//! `--record-corpus-item <id>` worker for the chosen item.
//!
//! Kept pure and free of egui so it unit-tests without a UI: [`parse_corpus`]
//! turns a manifest string into [`CorpusItem`]s, and [`resolve_corpus_path`] /
//! [`recorded_audio_path`] are thin filesystem-path helpers (mirroring the
//! Python `vp_benchmark_paths` resolution order: app-root → appdata).

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// One corpus item as the picker needs it: the `id` (the value passed to
/// `--record-corpus-item`), the full reference `text` the user reads aloud, and
/// the `language` tag (shown as context). Extra manifest keys (category, terms,
/// audio) are ignored here — the picker doesn't surface them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(in crate::ui) struct CorpusItem {
    pub(in crate::ui) id: String,
    pub(in crate::ui) text: String,
    #[serde(default)]
    pub(in crate::ui) language: String,
}

#[derive(Debug, Deserialize)]
struct CorpusManifest {
    #[serde(default)]
    items: Vec<RawItem>,
}

#[derive(Debug, Deserialize)]
struct RawItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    language: String,
}

/// Parse a `corpus.json` manifest string into the picker's [`CorpusItem`]s.
///
/// Returns an empty list (never an error / panic) when the manifest is missing
/// an `items` array or is malformed JSON — the picker then simply shows nothing
/// to record, exactly as if the file were absent. Items with a blank `id` or
/// blank `text` are skipped (they can't be recorded / read aloud).
pub(in crate::ui) fn parse_corpus(manifest: &str) -> Vec<CorpusItem> {
    let parsed: CorpusManifest = match serde_json::from_str(manifest) {
        Ok(manifest) => manifest,
        Err(_) => return Vec::new(),
    };
    parsed
        .items
        .into_iter()
        .filter_map(|raw| {
            let id = raw.id.trim().to_owned();
            let text = raw.text.trim().to_owned();
            if id.is_empty() || text.is_empty() {
                return None;
            }
            Some(CorpusItem {
                id,
                text,
                language: raw.language.trim().to_owned(),
            })
        })
        .collect()
}

/// A short, single-line preview of an item's reference text for the combo label,
/// truncated to ~`max_chars` characters with an ellipsis. Char-boundary safe
/// (truncates on `char` count, not bytes) so Danish text never panics.
pub(in crate::ui) fn text_preview(text: &str, max_chars: usize) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flattened.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Resolve the corpus manifest path the way the worker does: `<app_root>/
/// benchmark/corpus.json` if it exists, else `<appdata>/benchmark/corpus.json`.
/// Returns the first existing candidate, or `None` when neither exists. Mirrors
/// `vp_benchmark_paths.resolve_corpus_manifest` (sans the explicit-arg case the
/// UI never uses).
pub(in crate::ui) fn resolve_corpus_path(app_root: &Path, appdata: &Path) -> Option<PathBuf> {
    for base in [app_root, appdata] {
        let candidate = base.join("benchmark").join("corpus.json");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// The per-user recording path for a corpus item: `<appdata>/benchmark/audio/
/// <id>.wav` — the SAME location the worker writes to and the benchmark reads
/// from. Used to mark already-recorded items in the picker.
pub(in crate::ui) fn recorded_audio_path(appdata: &Path, id: &str) -> PathBuf {
    appdata
        .join("benchmark")
        .join("audio")
        .join(format!("{id}.wav"))
}

/// Whether a recording already exists for `id` under `appdata`. A thin
/// `exists()` check on [`recorded_audio_path`] so the picker can suffix a "✓".
pub(in crate::ui) fn has_recording(appdata: &Path, id: &str) -> bool {
    recorded_audio_path(appdata, id).exists()
}

/// The combo label for a corpus item: `id — <preview>` plus a ` ✓ (recorded)`
/// marker when a recording already exists under `appdata`. The localized
/// "recorded" word is supplied by the caller (kept here so the label is built in
/// one place and unit-tested without a UI). The preview is truncated to ~40
/// chars (char-boundary safe).
pub(in crate::ui) fn combo_entry_label(
    item: &CorpusItem,
    appdata: &Path,
    raw_language: &str,
) -> String {
    let preview = text_preview(&item.text, 40);
    let mut label = format!("{} — {preview}", item.id);
    if has_recording(appdata, &item.id) {
        let recorded = crate::ui::corpus_record_text(
            raw_language,
            crate::ui::CorpusRecordText::RecordedMarker,
        );
        label.push_str(&format!(" ✓ ({recorded})"));
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "version": 1,
        "audio_dir": "audio",
        "items": [
            {"id": "da-001", "language": "da", "text": "Hej med dig.", "terms": []},
            {"id": "en-001", "language": "en", "text": "Hello there, friend.", "terms": ["friend"]}
        ]
    }"#;

    #[test]
    fn parses_items_with_id_text_language() {
        let items = parse_corpus(SAMPLE);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "da-001");
        assert_eq!(items[0].text, "Hej med dig.");
        assert_eq!(items[0].language, "da");
        assert_eq!(items[1].id, "en-001");
        assert_eq!(items[1].language, "en");
    }

    #[test]
    fn missing_file_contents_yield_empty_no_panic() {
        // Simulates a missing/empty manifest: empty string → no items, no panic.
        assert!(parse_corpus("").is_empty());
    }

    #[test]
    fn malformed_json_yields_empty_no_panic() {
        assert!(parse_corpus("{not valid json").is_empty());
        assert!(parse_corpus("[]").is_empty()); // array, not the {items:[]} object
    }

    #[test]
    fn manifest_without_items_array_is_empty() {
        assert!(parse_corpus(r#"{"version":1}"#).is_empty());
    }

    #[test]
    fn skips_items_missing_id_or_text() {
        let manifest = r#"{"items":[
            {"id":"","text":"no id"},
            {"id":"ok","text":""},
            {"id":"good","text":"keep me"}
        ]}"#;
        let items = parse_corpus(manifest);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "good");
    }

    #[test]
    fn text_preview_truncates_and_collapses_whitespace() {
        let preview = text_preview("Hello   there\n  friend, this is long", 11);
        assert_eq!(preview, "Hello there…");
    }

    #[test]
    fn text_preview_keeps_short_text_whole() {
        assert_eq!(text_preview("Short", 40), "Short");
    }

    #[test]
    fn text_preview_is_char_boundary_safe_for_danish() {
        // Truncating mid-"æøå" must not panic (char count, not byte slice).
        let preview = text_preview("æøå æøå æøå", 4);
        assert_eq!(preview.chars().filter(|c| *c != '…').count(), 4);
    }

    #[test]
    fn recorded_audio_path_matches_worker_layout() {
        let appdata = Path::new("/home/u/.config/whisper-dictate");
        let path = recorded_audio_path(appdata, "da-001");
        assert!(path.ends_with("benchmark/audio/da-001.wav"));
    }

    #[test]
    fn resolve_prefers_app_root_then_falls_back_to_appdata() {
        let tmp = std::env::temp_dir().join(format!("wd-corpus-{}", std::process::id()));
        let app_root = tmp.join("app");
        let appdata = tmp.join("appdata");
        let _ = std::fs::remove_dir_all(&tmp);

        // Neither base has a manifest yet → None.
        assert!(resolve_corpus_path(&app_root, &appdata).is_none());

        // Only appdata has it → resolves there.
        let appdata_manifest = appdata.join("benchmark").join("corpus.json");
        std::fs::create_dir_all(appdata_manifest.parent().unwrap()).unwrap();
        std::fs::write(&appdata_manifest, "{}").unwrap();
        assert_eq!(
            resolve_corpus_path(&app_root, &appdata),
            Some(appdata_manifest)
        );

        // App-root also has it → app-root wins (resolution order).
        let app_manifest = app_root.join("benchmark").join("corpus.json");
        std::fs::create_dir_all(app_manifest.parent().unwrap()).unwrap();
        std::fs::write(&app_manifest, "{}").unwrap();
        assert_eq!(resolve_corpus_path(&app_root, &appdata), Some(app_manifest));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn has_recording_reflects_the_audio_file_presence() {
        let tmp = std::env::temp_dir().join(format!("wd-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(!has_recording(&tmp, "da-001"));

        let wav = recorded_audio_path(&tmp, "da-001");
        std::fs::create_dir_all(wav.parent().unwrap()).unwrap();
        std::fs::write(&wav, b"RIFF").unwrap();
        assert!(has_recording(&tmp, "da-001"));

        // The ✓ marker is added to the combo label when a recording exists.
        let item = CorpusItem {
            id: "da-001".to_owned(),
            text: "Hej med dig.".to_owned(),
            language: "da".to_owned(),
        };
        let label = combo_entry_label(&item, &tmp, "en");
        assert!(label.starts_with("da-001 — Hej med dig."));
        assert!(label.contains("✓"));
        assert!(label.contains("recorded"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
