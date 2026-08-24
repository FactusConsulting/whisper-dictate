//! Dictionary loading, caching, and live-reload state for runtime consumers.
//!
//! The command/RPC entry points stay in [`super::runtime`]; this module owns
//! the session snapshot and per-utterance reload provider so loading policy,
//! cache invalidation, and partial-failure handling have one boundary.

use std::path::{Path, PathBuf};

use super::runtime_settings::RuntimeDictionarySettings;
use super::store::load_dictionary;
use super::Dictionary;

/// The dictionary state a live in-process session needs: the loaded
/// [`Dictionary`] (for the replacement table) plus the resolved prompt-budget
/// knobs (for the Whisper `initial_prompt`). Built from the same
/// `VOICEPI_DICTIONARY*` env + `config.json` the `dictionary-runtime` RPC and
/// the Python worker read, so the in-process Rust engine biases + rewrites
/// identically.
#[derive(Debug, Clone)]
pub struct SessionDictionary {
    /// The merged dictionary (empty when disabled or nothing loaded).
    pub dictionary: Dictionary,
    /// Prompt term-count budget (`VOICEPI_DICTIONARY_MAX_TERMS`).
    pub max_terms: usize,
    /// Prompt character budget (`VOICEPI_DICTIONARY_PROMPT_CHARS`).
    pub max_chars: usize,
    /// Whether the dictionary is enabled (`VOICEPI_DICTIONARY_ENABLED`).
    pub enabled: bool,
}

impl SessionDictionary {
    /// Build the Whisper `initial_prompt` from `base_prompt` + the
    /// budget-fitted vocabulary terms, or `None` when both are empty (the
    /// caller then passes the empty string through). Mirrors Python's
    /// `_dictionary_prompt_runtime`.
    pub fn initial_prompt(&self, base_prompt: Option<&str>) -> Option<String> {
        self.dictionary
            .build_prompt(base_prompt, self.max_terms, self.max_chars)
    }

    /// `true` when the loaded dictionary carries any replacements, so the
    /// session wiring can skip attaching the replacement seam otherwise.
    pub fn has_replacements(&self) -> bool {
        !self.dictionary.replacements.is_empty()
    }

    /// Fold the dictionary terms into an existing prompt `slot` in place: take
    /// the current base prompt out, rebuild it through [`Self::initial_prompt`],
    /// and write the (possibly `None`) result back. Collapses the identical
    /// "take → initial_prompt → store" dance each backend-config call site
    /// (cloud `prompt`, local `initial_prompt`) would otherwise repeat, so the
    /// prompt-biasing wiring lives in exactly one place.
    pub fn fold_into_prompt(&self, slot: &mut Option<String>) {
        let base = slot.take();
        *slot = self.initial_prompt(base.as_deref());
    }
}

/// Load the [`SessionDictionary`] from the process env + `config.json`, the
/// single entry the in-process session uses for BOTH halves of dictionary
/// support: term-based prompt biasing ([`SessionDictionary::initial_prompt`])
/// and the replacement table ([`SessionDictionary::dictionary`]). When
/// disabled, returns an empty dictionary so both halves are no-ops.
pub fn load_session_dictionary() -> SessionDictionary {
    let settings = RuntimeDictionarySettings::from_env_and_config();
    load_session_dictionary_for(&settings)
}

/// Load a session dictionary from a caller-owned, already-resolved runtime
/// lookup. Empty suppression markers therefore stay authoritative instead of
/// falling back to ambient process variables in specialized consumers such as
/// the benchmark runner.
pub(crate) fn load_session_dictionary_with(
    lookup: &impl Fn(&str) -> Option<String>,
) -> SessionDictionary {
    let settings = RuntimeDictionarySettings::new(
        lookup("VOICEPI_DICTIONARY_ENABLED")
            .map(|value| {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "" | "0" | "false" | "no" | "off"
                )
            })
            .unwrap_or(true),
        lookup("VOICEPI_DICTIONARY")
            .map(|value| {
                std::env::split_paths(&value)
                    .filter(|path| !path.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        lookup("VOICEPI_DICTIONARY_MAX_TERMS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(80),
        lookup("VOICEPI_DICTIONARY_PROMPT_CHARS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1200),
    );
    load_session_dictionary_for(&settings)
}

fn load_session_dictionary_for(settings: &RuntimeDictionarySettings) -> SessionDictionary {
    let dictionary = load_dictionary_for(settings);
    SessionDictionary {
        dictionary,
        max_terms: settings.max_terms,
        max_chars: settings.max_chars,
        enabled: settings.enabled,
    }
}

/// Outcome of a [`load_dictionary_checked`] load, deciding how the caching
/// [`ReloadingDictionary`] treats the result.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DictionaryLoad {
    /// Every configured file read cleanly (or the dictionary is disabled / all
    /// paths are simply absent): the returned table is authoritative.
    Clean,
    /// Some file failed but at least one loaded successfully: the returned table
    /// is the readable subset (which may be legitimately EMPTY, e.g. a cleared
    /// file), usable now but not authoritative -- the caller should not advance
    /// its cache key so the failed file is retried.
    Partial,
    /// No file loaded successfully (every existing path was unreadable): the
    /// returned table is a fallback empty and must NOT replace a last-good one.
    TotalFailure,
}

/// Load the merged replacement [`Dictionary`] for the given settings: the
/// merged file contents when enabled, or an empty dictionary (no-op) when
/// disabled. Shared by [`load_session_dictionary`] and [`ReloadingDictionary`]
/// so both resolve the enabled/disabled + merge semantics identically.
fn load_dictionary_for(settings: &RuntimeDictionarySettings) -> Dictionary {
    load_dictionary_checked(settings).0
}

/// Like [`load_dictionary_for`] but also reports the [`DictionaryLoad`] outcome
/// so a caching caller can tell a clean load, a partial one (keep the readable
/// subset, retry the rest), and a total failure (keep last-good) apart. The
/// signal is which files LOADED (`loaded_paths`), not whether the merged table
/// is non-empty, so clearing the last readable dictionary still takes effect
/// even while a configured sibling is unreadable.
fn load_dictionary_checked(
    settings: &RuntimeDictionarySettings,
) -> (Dictionary, DictionaryLoad, Option<String>) {
    if !settings.enabled {
        return (Dictionary::default(), DictionaryLoad::Clean, None);
    }
    let (dictionary, loaded_paths, error) = load_runtime_dictionary(&settings.paths);
    let outcome = if error.is_none() {
        DictionaryLoad::Clean
    } else if loaded_paths.is_empty() {
        DictionaryLoad::TotalFailure
    } else {
        DictionaryLoad::Partial
    };
    (dictionary, outcome, error)
}

/// Per-utterance dictionary data for transcript replacements.
pub trait DictionaryProvider {
    /// The replacement table to apply to THIS utterance's transcript. May
    /// reload from disk/env; a static impl just returns its fixed table.
    fn current(&mut self) -> &Dictionary;

    /// Return and clear a load error observed while refreshing this provider.
    fn take_load_error(&mut self) -> Option<String> {
        None
    }

    /// Replace the provider's session-owned live settings. Environment-backed
    /// providers keep their historical behavior via this default no-op.
    fn apply_settings(&mut self, _settings: &std::collections::BTreeMap<String, String>) {}
}

/// A fixed dictionary snapshot for sessions and tests.
pub struct StaticDictionary(pub Dictionary);

impl DictionaryProvider for StaticDictionary {
    fn current(&mut self) -> &Dictionary {
        &self.0
    }
}

/// Which source wins when a [`ReloadingDictionary`] re-resolves its settings.
/// The two live callers differ deliberately:
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadPrecedence {
    /// `config.json` wins over the process env -- the live worker session
    /// (`make_real_session`), where a Settings save is the source of truth and
    /// the startup env is a now-stale mirror. Matches
    /// [`crate::config::worker_env_overrides`] and Python's `apply_config_to_environ`.
    ConfigFirst,
    /// The process env wins over `config.json` -- the env-driven
    /// `simulate-session` CLI verb (which reads every setting from the same
    /// `VOICEPI_*` env the worker command exports) and the `dictionary-runtime`
    /// RPC (where the caller passes the resolved value in via env).
    EnvFirst,
}

/// Cache key deciding whether the on-disk / env dictionary state changed since
/// the last utterance -- a Rust port of Python's `_dictionary_cache_key`: the
/// enable flag + resolved paths + prompt budgets, plus each configured file's
/// `(mtime_ns, size)` freshness stamp (`None` when the path does not exist).
/// Equality means "nothing that affects the table changed", so the reload can
/// be skipped.
#[derive(Clone, PartialEq, Eq)]
struct DictionaryReloadKey {
    enabled: bool,
    paths: Vec<PathBuf>,
    max_terms: usize,
    max_chars: usize,
    freshness: Vec<Option<(u128, u64)>>,
}

impl DictionaryReloadKey {
    /// Read the current settings under the given `precedence` and stamp each
    /// configured path, returning both the settings (so the caller can reload
    /// without re-reading env) and the key built from them. `None` only when a
    /// ConfigFirst resolve hits a present-but-unreadable config.json (a
    /// transient failure -- the caller keeps its last-good state and retries).
    fn resolve(precedence: ReloadPrecedence) -> Option<(RuntimeDictionarySettings, Self)> {
        let settings = match precedence {
            ReloadPrecedence::ConfigFirst => RuntimeDictionarySettings::from_config_and_env()?,
            ReloadPrecedence::EnvFirst => RuntimeDictionarySettings::from_env_and_config(),
        };
        let freshness = settings.paths.iter().map(|p| file_stamp(p)).collect();
        let key = Self {
            enabled: settings.enabled,
            paths: settings.paths.clone(),
            max_terms: settings.max_terms,
            max_chars: settings.max_chars,
            freshness,
        };
        Some((settings, key))
    }
}

/// `(mtime_ns, size)` for `path`, or `None` when it does not exist / cannot be
/// stat-ed. A changed modification time OR size flips the cache key, so a live
/// edit (even one that keeps the byte length) is caught by the nanosecond
/// mtime.
fn file_stamp(path: &Path) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

/// A [`DictionaryProvider`] that live-reloads the replacement table: each
/// [`Self::current`] recomputes the [`DictionaryReloadKey`] and reloads from
/// disk only on a miss. Mirrors Python's `_dictionary_runtime`, which re-reads
/// per utterance behind the same mtime+settings cache -- so a user editing
/// their dictionary file or toggling `VOICEPI_DICTIONARY_ENABLED` sees the
/// change on the next utterance without restarting the app.
pub struct ReloadingDictionary {
    /// Which source wins when re-resolving the settings each utterance.
    precedence: ReloadPrecedence,
    /// Explicit settings supplied by the native in-process session. When
    /// present, reloads only restat/read dictionary files; they never consult
    /// process-global configuration or environment variables.
    owned_settings: Option<RuntimeDictionarySettings>,
    /// The key of the last SUCCESSFUL load, or `None` until one succeeds. Kept
    /// as an `Option` (rather than the key of whatever was last attempted) so a
    /// failed load never advances it -- the next utterance recomputes the key,
    /// finds it still differs, and retries instead of caching the failure.
    key: Option<DictionaryReloadKey>,
    /// The last successfully-loaded table (empty until the first success).
    dictionary: Dictionary,
    /// Prompt budgets from the last successful load, so [`Self::initial_prompt`]
    /// can budget-fit the vocabulary terms without re-reading settings.
    max_terms: usize,
    max_chars: usize,
    load_error: Option<String>,
}

impl ReloadingDictionary {
    /// Best-effort initial load under `precedence` so the first utterance
    /// already reflects the on-disk state; a failed initial load leaves
    /// `key == None` so the next [`Self::current`] retries.
    pub fn new(precedence: ReloadPrecedence) -> Self {
        let mut provider = Self {
            precedence,
            owned_settings: None,
            key: None,
            dictionary: Dictionary::default(),
            max_terms: 80,
            max_chars: 1200,
            load_error: None,
        };
        provider.current();
        provider
    }

    /// Build a file-reloading provider from a typed session snapshot.
    #[cfg(any(all(feature = "whisper-rs-local", feature = "rust-injection"), test))]
    pub(crate) fn from_settings(settings: RuntimeDictionarySettings) -> Self {
        let mut provider = Self {
            precedence: ReloadPrecedence::ConfigFirst,
            owned_settings: Some(settings),
            key: None,
            dictionary: Dictionary::default(),
            max_terms: 80,
            max_chars: 1200,
            load_error: None,
        };
        provider.current();
        provider
    }

    /// The current STT `initial_prompt`: `base` plus the budget-fitted
    /// vocabulary terms, reloaded via the same mtime/settings cache as
    /// [`Self::current`]. `None` when there is neither a base nor any terms (the
    /// caller then passes the empty string through). Mirrors Python's
    /// per-utterance `_dictionary_prompt_runtime`, so editing the dictionary's
    /// terms (or the prompt budgets) re-biases STT on the next utterance without
    /// an app restart.
    pub fn initial_prompt(&mut self, base: Option<&str>) -> Option<String> {
        self.current();
        self.dictionary
            .build_prompt(base, self.max_terms, self.max_chars)
    }

    pub fn initial_prompt_with_terms(
        &mut self,
        base: Option<&str>,
    ) -> (Option<String>, Vec<String>) {
        self.current();
        let terms = self.dictionary.prompt_terms(self.max_terms, self.max_chars);
        let prompt = self
            .dictionary
            .build_prompt(base, self.max_terms, self.max_chars);
        (prompt, terms)
    }
}

impl DictionaryProvider for ReloadingDictionary {
    fn current(&mut self) -> &Dictionary {
        // A `None` resolve means the config file is present but unreadable (a
        // transient failure, e.g. a Settings save caught mid-rewrite): keep the
        // last-good table and retry next utterance.
        let resolved = self
            .owned_settings
            .clone()
            .map(|settings| {
                let key = DictionaryReloadKey {
                    enabled: settings.enabled,
                    paths: settings.paths.clone(),
                    max_terms: settings.max_terms,
                    max_chars: settings.max_chars,
                    freshness: settings.paths.iter().map(|path| file_stamp(path)).collect(),
                };
                (settings, key)
            })
            .or_else(|| DictionaryReloadKey::resolve(self.precedence));
        if let Some((settings, key)) = resolved {
            if self.key.as_ref() != Some(&key) {
                match load_dictionary_checked(&settings) {
                    (dictionary, DictionaryLoad::Clean, _) => {
                        // Clean load: commit the table + budgets AND advance the
                        // cache key.
                        self.dictionary = dictionary;
                        self.max_terms = settings.max_terms;
                        self.max_chars = settings.max_chars;
                        self.key = Some(key);
                        self.load_error = None;
                    }
                    (dictionary, DictionaryLoad::Partial, error) => {
                        // Some files failed but at least one loaded (its subset
                        // may be legitimately empty, e.g. a cleared file). Use
                        // it now, but leave the key UNADVANCED so the failed file
                        // is retried next utterance.
                        self.dictionary = dictionary;
                        self.max_terms = settings.max_terms;
                        self.max_chars = settings.max_chars;
                        self.load_error = error;
                    }
                    (_, DictionaryLoad::TotalFailure, error) => {
                        // Nothing loaded (every existing file is momentarily
                        // unreadable): keep the last-good table and retry.
                        self.load_error = error;
                    }
                }
            }
        }
        &self.dictionary
    }

    fn take_load_error(&mut self) -> Option<String> {
        self.load_error.take()
    }

    fn apply_settings(&mut self, settings: &std::collections::BTreeMap<String, String>) {
        if let Some(owned) = self.owned_settings.as_mut() {
            owned.update_from_live_values(settings);
        }
    }
}

pub(crate) fn load_runtime_dictionary(
    paths: &[PathBuf],
) -> (Dictionary, Vec<PathBuf>, Option<String>) {
    let mut dictionary = Dictionary::default();
    let mut loaded_paths = Vec::new();
    let mut error = None;

    for path in paths {
        if !path.exists() {
            continue;
        }
        match load_dictionary(path) {
            Ok(next) => {
                merge_dictionary(&mut dictionary, next);
                loaded_paths.push(path.clone());
            }
            Err(err) => append_error(&mut error, format!("{}: {err}", path.display())),
        }
    }

    dictionary.terms = super::dedupe_terms(dictionary.terms);
    (dictionary, loaded_paths, error)
}

fn merge_dictionary(into: &mut Dictionary, next: Dictionary) {
    into.terms.extend(next.terms);
    for replacement in next.replacements {
        if let Some(existing) = into
            .replacements
            .iter_mut()
            .find(|existing| existing.from == replacement.from)
        {
            existing.to = replacement.to;
        } else {
            into.replacements.push(replacement);
        }
    }
}

pub(crate) fn append_error(target: &mut Option<String>, message: String) {
    if message.trim().is_empty() {
        return;
    }
    match target {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&message);
        }
        None => *target = Some(message),
    }
}

#[cfg(test)]
#[path = "runtime_loader_tests.rs"]
mod tests;
