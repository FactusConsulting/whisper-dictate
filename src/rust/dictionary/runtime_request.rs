//! JSON request/response entry points for the hidden `dictionary-runtime` RPC.
//!
//! The CLI command handler remains in [`super::runtime`]. This module owns the
//! wire format and the in-process equivalent so callers share one request
//! contract regardless of whether the dictionary is reached through stdin or
//! directly from the native runtime.

use std::io::{self, Read};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::runtime_loader::{append_error, load_runtime_dictionary};
use super::runtime_settings::RuntimeDictionarySettings;
use super::{Dictionary, Replacement, ReplacementChange};

#[derive(Debug, Deserialize)]
struct RuntimeRequest {
    #[serde(default)]
    base_prompt: Option<String>,
    #[serde(default)]
    text: String,
}

/// The wire-format response from `dictionary-runtime` (and the in-process
/// equivalent [`runtime_dictionary_result`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeDictionaryResult {
    pub enabled: bool,
    pub path: Option<String>,
    pub loaded_paths: Vec<String>,
    pub term_count: usize,
    pub replacement_count: usize,
    pub terms: Vec<String>,
    pub all_terms: Vec<String>,
    pub replacements: Vec<Replacement>,
    pub prompt: Option<String>,
    pub text: String,
    pub changes: Vec<ReplacementChange>,
    pub error: Option<String>,
}

/// Read a JSON request from stdin, build the prompt + apply replacements, then
/// print the JSON response on stdout. Used by the Python worker to skip its
/// own dictionary loader when the Rust binary is available.
pub fn handle_runtime() -> Result<()> {
    let request = read_runtime_request()?;
    let settings = RuntimeDictionarySettings::from_env_and_config();
    let result =
        runtime_dictionary_result(&settings, request.base_prompt.as_deref(), &request.text);
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// In-process equivalent of [`handle_runtime`] — same shape, but the caller
/// supplies the settings + request directly (used by native runtime callers
/// and unit tests).
pub fn runtime_dictionary_result(
    settings: &RuntimeDictionarySettings,
    base_prompt: Option<&str>,
    text: &str,
) -> RuntimeDictionaryResult {
    let path = settings
        .paths
        .first()
        .map(|path| path.display().to_string());
    if !settings.enabled {
        let dictionary = Dictionary::default();
        return RuntimeDictionaryResult {
            enabled: false,
            path,
            loaded_paths: Vec::new(),
            term_count: 0,
            replacement_count: 0,
            terms: Vec::new(),
            all_terms: Vec::new(),
            replacements: Vec::new(),
            prompt: dictionary.build_prompt(base_prompt, settings.max_terms, settings.max_chars),
            text: text.to_owned(),
            changes: Vec::new(),
            error: None,
        };
    }

    let (dictionary, loaded_paths, mut error) = load_runtime_dictionary(&settings.paths);
    let terms = dictionary.prompt_terms(settings.max_terms, settings.max_chars);
    let prompt = dictionary.build_prompt(base_prompt, settings.max_terms, settings.max_chars);
    let all_terms = dictionary.terms.clone();
    let replacements = dictionary.replacements.clone();
    let (text, changes) = match dictionary.apply_replacements(text) {
        Ok(result) => result,
        Err(err) => {
            append_error(&mut error, err.to_string());
            (text.to_owned(), Vec::new())
        }
    };

    RuntimeDictionaryResult {
        enabled: true,
        path,
        loaded_paths: loaded_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
        term_count: dictionary.terms.len(),
        replacement_count: dictionary.replacements.len(),
        terms,
        all_terms,
        replacements,
        prompt,
        text,
        changes,
        error,
    }
}

fn read_runtime_request() -> Result<RuntimeRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
#[path = "runtime_request_tests.rs"]
mod tests;
