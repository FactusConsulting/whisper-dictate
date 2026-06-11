//! Dictionary, history, and metrics helpers: create/open the backing files and
//! render bounded text previews into the settings status area.

use super::*;
use crate::{dictionary, telemetry};

impl WhisperDictateApp {
    pub(in crate::ui) fn ensure_dictionary(&mut self) {
        match config::ensure_dictionary_file(&self.settings.dictionary) {
            Ok(path) => {
                self.settings_status = format!("Dictionary ready: {}", path.display());
            }
            Err(err) => {
                self.settings_status = format!("Dictionary create failed: {err}");
            }
        }
    }

    pub(in crate::ui) fn open_dictionary(&mut self) {
        match config::open_dictionary(&self.settings.dictionary) {
            Ok(path) => {
                self.settings_status = format!("Opened dictionary: {}", path.display());
            }
            Err(err) => {
                self.settings_status = format!("Open dictionary failed: {err}");
            }
        }
    }

    fn history_path(&self) -> std::path::PathBuf {
        if self.settings.history_jsonl.trim().is_empty() {
            config::default_history_path()
        } else {
            std::path::PathBuf::from(self.settings.history_jsonl.trim())
        }
    }

    pub(in crate::ui) fn preview_history(&mut self) {
        let path = self.history_path();
        match telemetry::preview_jsonl(&path, 20) {
            Ok(preview) => {
                self.history_preview = format!(
                    "{}\nrows: showing {} of {}\n{}",
                    preview.path.display(),
                    preview.shown_rows,
                    preview.total_rows,
                    preview.text
                );
                self.settings_status = format!("Loaded history preview: {}", path.display());
                // Scroll the just-loaded preview into view on its next render
                // (it sits below the settings ScrollArea fold otherwise).
                self.scroll_to_history_preview = true;
            }
            Err(err) => {
                self.history_preview.clear();
                self.settings_status = format!("History preview failed: {err}");
            }
        }
    }

    pub(in crate::ui) fn open_history(&mut self) {
        let path = self.history_path();
        match config::open_existing_path(&path) {
            Ok(path) => self.settings_status = format!("Opened history: {}", path.display()),
            Err(err) => self.settings_status = format!("Open history failed: {err}"),
        }
    }

    pub(in crate::ui) fn preview_metrics(&mut self) {
        let raw = self.settings.metrics_jsonl.trim();
        if raw.is_empty() {
            self.metrics_preview.clear();
            self.settings_status = "Metrics JSONL path is unset.".to_owned();
            return;
        }
        let path = std::path::PathBuf::from(raw);
        match telemetry::preview_jsonl(&path, 20) {
            Ok(preview) => {
                self.metrics_preview = format!(
                    "{}\nrows: showing {} of {}\n{}",
                    preview.path.display(),
                    preview.shown_rows,
                    preview.total_rows,
                    preview.text
                );
                self.settings_status = format!("Loaded metrics preview: {}", path.display());
                // Scroll the just-loaded preview into view on its next render
                // (it sits below the settings ScrollArea fold otherwise).
                self.scroll_to_metrics_preview = true;
            }
            Err(err) => {
                self.metrics_preview.clear();
                self.settings_status = format!("Metrics preview failed: {err}");
            }
        }
    }

    pub(in crate::ui) fn open_metrics(&mut self) {
        let raw = self.settings.metrics_jsonl.trim();
        if raw.is_empty() {
            self.settings_status = "Metrics JSONL path is unset.".to_owned();
            return;
        }
        match config::open_existing_path(raw) {
            Ok(path) => self.settings_status = format!("Opened metrics: {}", path.display()),
            Err(err) => self.settings_status = format!("Open metrics failed: {err}"),
        }
    }

    pub(in crate::ui) fn preview_dictionary(&mut self) {
        let max_terms = self
            .settings
            .dictionary_max_terms
            .parse::<usize>()
            .unwrap_or(80);
        let max_chars = self
            .settings
            .dictionary_prompt_chars
            .parse::<usize>()
            .unwrap_or(1200);
        match dictionary::preview_dictionary(
            self.settings.dictionary.clone(),
            Some(&self.settings.initial_prompt),
            max_terms,
            max_chars,
        ) {
            Ok(preview) => {
                self.settings_status = format!(
                    "Dictionary preview: {} terms, {} replacements",
                    preview.term_count, preview.replacement_count
                );
                self.dictionary_preview = preview.prompt.unwrap_or_else(|| {
                    "(No prompt terms selected by current dictionary limits)".to_owned()
                });
            }
            Err(err) => {
                self.settings_status = format!("Dictionary preview failed: {err}");
                self.dictionary_preview.clear();
            }
        }
    }
}
