//! `dictionary` and `dictionary-runtime` CLI command handlers.
//!
//! `dictionary` exposes the user-facing read/add operations
//! (`status`, `open`, `add`, `replace`). `dictionary-runtime` is the hidden
//! JSON-on-stdin RPC the Python worker calls to build the Whisper
//! `initial_prompt` and apply post-STT replacements without going through the
//! Python parser. Both go through [`RuntimeDictionarySettings`] which reads
//! env vars first (`VOICEPI_DICTIONARY*`) then `config.json` so the user can
//! override anything from the shell.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::DictionaryCommand;
use crate::config;

use super::store::load_dictionary;
use super::{env_bool, env_paths, env_usize, Dictionary, Replacement, ReplacementChange};

#[derive(Debug, Deserialize)]
struct RuntimeRequest {
    #[serde(default)]
    base_prompt: Option<String>,
    #[serde(default)]
    text: String,
}

/// Effective settings used by the `dictionary-runtime` handler. Env vars win
/// over `config.json`; missing values fall back to the defaults baked into the
/// Python side so the Python and Rust runtimes stay byte-identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDictionarySettings {
    pub enabled: bool,
    pub paths: Vec<PathBuf>,
    pub max_terms: usize,
    pub max_chars: usize,
}

impl RuntimeDictionarySettings {
    pub fn new(enabled: bool, paths: Vec<PathBuf>, max_terms: usize, max_chars: usize) -> Self {
        Self {
            enabled,
            paths,
            max_terms,
            max_chars,
        }
    }

    /// Resolve the effective settings ENV-FIRST: the process env wins over
    /// `config.json`, then the baked defaults. Used by the `dictionary-runtime`
    /// RPC and the env-driven `simulate-session` verb, where the caller passes
    /// the resolved value in via env.
    fn from_env_and_config() -> Self {
        let configured = config::load_settings().unwrap_or_default();
        Self::new(
            env_bool("VOICEPI_DICTIONARY_ENABLED").unwrap_or(configured.dictionary_enabled),
            env_paths("VOICEPI_DICTIONARY").unwrap_or_else(|| config_dictionary_paths(&configured)),
            env_usize("VOICEPI_DICTIONARY_MAX_TERMS")
                .or_else(|| configured.dictionary_max_terms.parse().ok())
                .unwrap_or(80),
            env_usize("VOICEPI_DICTIONARY_PROMPT_CHARS")
                .or_else(|| configured.dictionary_prompt_chars.parse().ok())
                .unwrap_or(1200),
        )
    }

    /// Resolve the effective settings CONFIG-FIRST for the live-reload path:
    /// for each dictionary key actually PRESENT in the raw `config.json`, config
    /// wins; for a key the file omits, the process env is the fallback (then the
    /// default). This keeps a saved Settings value authoritative over the stale
    /// startup env (the worker exports these once) -- the #555 P1 fix and the
    /// resolution-side equivalent of Python's `apply_config_to_environ` -- while
    /// still honouring an explicit env override for a key the config omits, so a
    /// partial/legacy config.json does not make the non-empty DEFAULT dictionary
    /// path shadow an env-supplied one.
    ///
    /// Returns `None` when the config file EXISTS but cannot be read/parsed -- a
    /// transient failure (e.g. a non-atomic Settings save caught mid-rewrite),
    /// so the caller keeps its last-good state and retries -- versus a MISSING
    /// file, which `load_raw_config` reports as `{}` (all keys absent -> env
    /// fallback), not an error.
    fn from_config_and_env() -> Option<Self> {
        let raw = config::load_raw_config().ok()?;
        let present = |key: &str| {
            raw.as_object()
                .map(|obj| obj.contains_key(key))
                .unwrap_or(false)
        };
        let (has_enabled, has_path, has_terms, has_chars) = (
            present("dictionary_enabled"),
            present("dictionary"),
            present("dictionary_max_terms"),
            present("dictionary_prompt_chars"),
        );
        // Read the enable flag straight off the RAW value (JSON bool OR string)
        // before the typed loader collapses it -- `AppSettings::from_value`'s
        // `bool_value` only parses string booleans, so a hand-written
        // `"dictionary_enabled": false` would otherwise fall back to the
        // default `true` and re-enable a disabled dictionary.
        let raw_enabled = raw_bool(&raw, "dictionary_enabled");
        let configured = config::AppSettings::from_value(raw).unwrap_or_default();

        let enabled = if has_enabled {
            raw_enabled.unwrap_or(configured.dictionary_enabled)
        } else {
            env_bool("VOICEPI_DICTIONARY_ENABLED").unwrap_or(configured.dictionary_enabled)
        };
        let paths = if has_path {
            config_dictionary_paths(&configured)
        } else {
            env_paths("VOICEPI_DICTIONARY").unwrap_or_else(|| config_dictionary_paths(&configured))
        };
        let max_terms = if has_terms {
            configured.dictionary_max_terms.parse().unwrap_or(80)
        } else {
            env_usize("VOICEPI_DICTIONARY_MAX_TERMS")
                .or_else(|| configured.dictionary_max_terms.parse().ok())
                .unwrap_or(80)
        };
        let max_chars = if has_chars {
            configured.dictionary_prompt_chars.parse().unwrap_or(1200)
        } else {
            env_usize("VOICEPI_DICTIONARY_PROMPT_CHARS")
                .or_else(|| configured.dictionary_prompt_chars.parse().ok())
                .unwrap_or(1200)
        };
        Some(Self::new(enabled, paths, max_terms, max_chars))
    }
}

/// The configured dictionary path(s), or empty when the config carries none (so
/// the caller can fall through to env / default). Splits the value on the
/// platform path separator the same way [`env_paths`] does, so a multi-file
/// `dictionary` (e.g. `a.json;b.json` on Windows) loads every file rather than
/// wrapping the whole list in one bogus `PathBuf`.
fn config_dictionary_paths(configured: &config::AppSettings) -> Vec<PathBuf> {
    let value = configured.dictionary.trim();
    if value.is_empty() {
        return Vec::new();
    }
    std::env::split_paths(value)
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

/// Read a `dictionary_enabled`-style flag from a raw config value, accepting a
/// JSON boolean, a JSON number (`0` = false), or a string (`"0"`/`"false"`/... =
/// false) -- the union the env resolver honours -- so a hand-written config.json
/// is respected regardless of how the flag is spelt. `None` when the key is
/// absent or an unrecognised shape.
fn raw_bool(raw: &Value, key: &str) -> Option<bool> {
    match raw.get(key) {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Number(number)) => number.as_i64().map(|n| n != 0),
        Some(Value::String(text)) => {
            let value = text.trim().to_ascii_lowercase();
            if value.is_empty() {
                None
            } else {
                Some(!matches!(value.as_str(), "0" | "false" | "no" | "off"))
            }
        }
        _ => None,
    }
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

/// Status preview emitted by the `dictionary status` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryPreview {
    pub path: PathBuf,
    pub term_count: usize,
    pub replacement_count: usize,
    pub prompt: Option<String>,
}

/// Build a [`DictionaryPreview`] for `path` against the given prompt + budgets.
pub fn preview_dictionary(
    path: impl Into<PathBuf>,
    base_prompt: Option<&str>,
    max_terms: usize,
    max_chars: usize,
) -> Result<DictionaryPreview> {
    let path = path.into();
    let dictionary = load_dictionary(&path)?;
    Ok(DictionaryPreview {
        path,
        term_count: dictionary.terms.len(),
        replacement_count: dictionary.replacements.len(),
        prompt: dictionary.build_prompt(base_prompt, max_terms, max_chars),
    })
}

/// Dispatch table for the user-facing `dictionary` subcommands.
pub fn handle_command(command: DictionaryCommand) -> Result<()> {
    let settings = dictionary_command_settings()?;
    let path = PathBuf::from(&settings.dictionary);
    match command {
        DictionaryCommand::Status => {
            let preview = if path.exists() {
                preview_dictionary(
                    &path,
                    Some(&settings.initial_prompt),
                    settings.dictionary_max_terms.parse().unwrap_or(80),
                    settings.dictionary_prompt_chars.parse().unwrap_or(1200),
                )?
            } else {
                let dictionary = Dictionary::default();
                DictionaryPreview {
                    path: path.clone(),
                    term_count: 0,
                    replacement_count: 0,
                    prompt: dictionary.build_prompt(
                        Some(&settings.initial_prompt),
                        settings.dictionary_max_terms.parse().unwrap_or(80),
                        settings.dictionary_prompt_chars.parse().unwrap_or(1200),
                    ),
                }
            };
            println!("path: {}", preview.path.display());
            println!("terms: {}", preview.term_count);
            println!("replacements: {}", preview.replacement_count);
            if let Some(prompt) = preview.prompt {
                println!("prompt:\n{prompt}");
            }
        }
        DictionaryCommand::Open => {
            let path = config::open_dictionary(path)?;
            println!("opened: {}", path.display());
        }
        DictionaryCommand::Add { term } => {
            let added = super::store::add_term(&path, &term)?;
            println!(
                "{}: {}",
                if added { "added" } else { "already present" },
                path.display()
            );
        }
        DictionaryCommand::Replace { mapping } => {
            let (from, to, changed) = super::store::add_replacement(&path, &mapping)?;
            println!(
                "{}: {from} => {to} ({})",
                if changed { "saved" } else { "unchanged" },
                path.display()
            );
        }
        DictionaryCommand::BuildFromCorpus {
            benchmark_corpus,
            app_root,
            dictionary,
            language,
            category,
            min_count,
            apply,
            json,
        } => {
            let opts = super::training::BuildFromCorpusOptions {
                corpus_manifest: benchmark_corpus,
                app_root: app_root.map(PathBuf::from),
                appdata: Some(config::platform_config_dir()),
                dictionary_path: dictionary,
                language,
                category,
                min_count,
                apply,
                as_json: json,
            };
            let rc = super::training::run_build_from_corpus(opts);
            if rc != 0 {
                std::process::exit(rc);
            }
        }
        DictionaryCommand::Prompt {
            dictionary,
            json,
            max_length,
        } => {
            super::prompt::handle_prompt(dictionary, json, max_length)?;
        }
        DictionaryCommand::List { dictionary, json } => {
            super::prompt::handle_list(dictionary, json)?;
        }
        DictionaryCommand::SuggestTerms {
            jsonl,
            dictionary,
            min_count,
            apply,
            json,
        } => {
            let opts = super::training::SuggestFromMissesOptions {
                jsonl_path: PathBuf::from(jsonl),
                dictionary_path: dictionary,
                min_count,
                apply,
                as_json: json,
            };
            let rc = super::training::run_suggest_from_misses(opts);
            if rc != 0 {
                std::process::exit(rc);
            }
        }
        DictionaryCommand::SuggestReplacements {
            jsonl,
            dictionary,
            min_confidence,
            json,
        } => {
            let opts = super::suggest::SuggestReplacementsOptions {
                jsonl_path: jsonl,
                dictionary_path: dictionary,
                min_confidence,
                as_json: json,
            };
            let rc = super::suggest::run_suggest_replacements(opts);
            if rc != 0 {
                std::process::exit(rc);
            }
        }
    }
    Ok(())
}

/// Public re-export of the private `dictionary_command_settings` helper so
/// the sibling `prompt` module can reuse the exact env / config precedence
/// used by `dictionary status`. Kept as a distinct name to make the
/// coupling obvious from `prompt.rs`.
pub(super) fn dictionary_command_settings_for_prompt() -> Result<config::AppSettings> {
    dictionary_command_settings()
}

fn dictionary_command_settings() -> Result<config::AppSettings> {
    let mut settings = config::load_settings()?;
    if let Some(paths) = env_paths("VOICEPI_DICTIONARY") {
        if let Some(path) = paths.first() {
            settings.dictionary = path.display().to_string();
        }
    }
    if let Some(enabled) = env_bool("VOICEPI_DICTIONARY_ENABLED") {
        settings.dictionary_enabled = enabled;
    }
    if let Some(value) = env_usize("VOICEPI_DICTIONARY_MAX_TERMS") {
        settings.dictionary_max_terms = value.to_string();
    }
    if let Some(value) = env_usize("VOICEPI_DICTIONARY_PROMPT_CHARS") {
        settings.dictionary_prompt_chars = value.to_string();
    }
    Ok(settings)
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
/// supplies the settings + request directly (used by unit tests).
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
    let dictionary = load_dictionary_for(&settings);
    SessionDictionary {
        dictionary,
        max_terms: settings.max_terms,
        max_chars: settings.max_chars,
        enabled: settings.enabled,
    }
}

/// Load the merged replacement [`Dictionary`] for the given settings: the
/// merged file contents when enabled, or an empty dictionary (no-op) when
/// disabled. Shared by [`load_session_dictionary`] and [`ReloadingDictionary`]
/// so both resolve the enabled/disabled + merge semantics identically.
fn load_dictionary_for(settings: &RuntimeDictionarySettings) -> Dictionary {
    load_dictionary_checked(settings).0
}

/// Like [`load_dictionary_for`] but also reports whether the load SUCCEEDED
/// (no file read/parse error). A disabled dictionary is a successful empty
/// table. `false` means the returned table is a fallback empty produced from a
/// failed read/parse, so a caller that caches (see [`ReloadingDictionary`])
/// must NOT treat it as authoritative -- otherwise a transient failure (e.g. a
/// Windows editor briefly locking the file) would replace the last-good table
/// with an empty one and cache it until the next mtime bump.
fn load_dictionary_checked(settings: &RuntimeDictionarySettings) -> (Dictionary, bool) {
    if !settings.enabled {
        return (Dictionary::default(), true);
    }
    let (dictionary, _loaded, error) = load_runtime_dictionary(&settings.paths);
    (dictionary, error.is_none())
}

/// A per-utterance source of the current replacement [`Dictionary`] for a
/// running [`crate::dictate::DictateSession`]. Mirrors Python's per-utterance
/// `_dictionary_runtime`: [`StaticDictionary`] returns a table fixed at
/// construction, while [`ReloadingDictionary`] re-reads the
/// `VOICEPI_DICTIONARY*` env + `config.json` + the dictionary file(s) each
/// utterance (cheap when unchanged, via an mtime+settings cache key) so live
/// edits take effect without an app restart.
pub trait DictionaryProvider {
    /// The replacement table to apply to THIS utterance's transcript. May
    /// reload from disk/env; a static impl just returns its fixed table.
    fn current(&mut self) -> &Dictionary;
}

/// A fixed replacement table (no reload). Backs
/// [`crate::dictate::DictateSession::with_dictionary`] and the session tests
/// so a caller with an already-loaded table keeps the pre-reload behaviour.
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
    /// [`config::worker_env_overrides`] and Python's `apply_config_to_environ`.
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
    /// The key of the last SUCCESSFUL load, or `None` until one succeeds. Kept
    /// as an `Option` (rather than the key of whatever was last attempted) so a
    /// failed load never advances it -- the next utterance recomputes the key,
    /// finds it still differs, and retries instead of caching the failure.
    key: Option<DictionaryReloadKey>,
    /// The last successfully-loaded table (empty until the first success).
    dictionary: Dictionary,
}

impl ReloadingDictionary {
    /// Best-effort initial load under `precedence` so the first utterance
    /// already reflects the on-disk state; a failed initial load leaves
    /// `key == None` so the next [`Self::current`] retries.
    pub fn new(precedence: ReloadPrecedence) -> Self {
        let mut provider = Self {
            precedence,
            key: None,
            dictionary: Dictionary::default(),
        };
        provider.current();
        provider
    }
}

impl DictionaryProvider for ReloadingDictionary {
    fn current(&mut self) -> &Dictionary {
        // A `None` resolve means the config file is present but unreadable (a
        // transient failure, e.g. a Settings save caught mid-rewrite): keep the
        // last-good table and retry next utterance.
        if let Some((settings, key)) = DictionaryReloadKey::resolve(self.precedence) {
            if self.key.as_ref() != Some(&key) {
                let (dictionary, ok) = load_dictionary_checked(&settings);
                if ok {
                    // Clean load: commit the table AND advance the cache key.
                    self.dictionary = dictionary;
                    self.key = Some(key);
                } else if !dictionary.replacements.is_empty() || !dictionary.terms.is_empty() {
                    // Partial load -- some configured files failed but others
                    // merged into a non-empty table (`load_runtime_dictionary`
                    // returns the readable subset alongside the error). Use that
                    // subset now, but leave the key UNADVANCED so the failed
                    // file is retried next utterance.
                    self.dictionary = dictionary;
                } else {
                    // Total failure (e.g. the only file is momentarily
                    // unreadable): keep the last-good table and retry.
                }
            }
        }
        &self.dictionary
    }
}

fn load_runtime_dictionary(paths: &[PathBuf]) -> (Dictionary, Vec<PathBuf>, Option<String>) {
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

fn append_error(target: &mut Option<String>, message: String) {
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

fn read_runtime_request() -> Result<RuntimeRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

// Keep the legacy `Path` import referenced for future-proofing callers that
// reach into the module's internals via `dictionary::runtime`.
#[allow(dead_code)]
fn _path_marker(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Snapshot every dictionary env var on construction and restore each to
    /// its prior value on drop -- Rust tests share one process env and run in
    /// arbitrary order, so a test that sets `VOICEPI_DICTIONARY*` must leave the
    /// environment exactly as it found it (restore-on-drop also fires during a
    /// panic, so a failed assertion can't leak). Hold alongside `ENV_LOCK`.
    struct DictEnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl DictEnvGuard {
        fn new() -> Self {
            // `VOICEPI_CONFIG` is included so config-first tests can point
            // `config::load_settings` at a temp config.json and have it restored
            // like the rest.
            let keys = [
                "VOICEPI_DICTIONARY",
                "VOICEPI_DICTIONARY_ENABLED",
                "VOICEPI_DICTIONARY_MAX_TERMS",
                "VOICEPI_DICTIONARY_PROMPT_CHARS",
                "VOICEPI_CONFIG",
            ];
            let saved = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            Self { saved }
        }
    }

    impl Drop for DictEnvGuard {
        fn drop(&mut self) {
            for (key, prior) in &self.saved {
                match prior {
                    Some(val) => std::env::set_var(key, val),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    /// Write a config.json at `config_path` whose dictionary points at
    /// `dict_path` with the given `enabled` flag (budgets left at their
    /// defaults), and export `VOICEPI_CONFIG` so `config::load_settings` reads
    /// it. Lets the config-first tests drive `dictionary` / `dictionary_enabled`
    /// through config.json -- the source of truth the live-reload now honours --
    /// rather than through env.
    fn write_dictionary_config(config_path: &Path, dict_path: &Path, enabled: bool) {
        let settings = config::AppSettings {
            dictionary: dict_path.display().to_string(),
            dictionary_enabled: enabled,
            ..config::AppSettings::default()
        };
        config::save_settings_to_path(&settings, config_path).expect("write temp config.json");
        std::env::set_var("VOICEPI_CONFIG", config_path);
    }

    #[test]
    fn session_dictionary_builds_prompt_and_reports_replacements() {
        // Pure: `initial_prompt` fits the base prompt + budget-limited terms,
        // and `has_replacements` reflects the table -- no env, no I/O.
        let sd = SessionDictionary {
            dictionary: Dictionary {
                terms: vec!["Codex".to_owned(), "Claude Code".to_owned()],
                replacements: vec![Replacement {
                    from: "code x".to_owned(),
                    to: "Codex".to_owned(),
                }],
            },
            max_terms: 80,
            max_chars: 1200,
            enabled: true,
        };
        assert!(sd.has_replacements());
        let prompt = sd
            .initial_prompt(Some("base hint"))
            .expect("prompt present");
        assert!(prompt.contains("base hint"), "{prompt}");
        assert!(
            prompt.contains("Vocabulary: Codex, Claude Code"),
            "{prompt}"
        );

        let empty = SessionDictionary {
            dictionary: Dictionary::default(),
            max_terms: 80,
            max_chars: 1200,
            enabled: false,
        };
        assert!(!empty.has_replacements());
        assert_eq!(empty.initial_prompt(None), None);
    }

    #[test]
    fn fold_into_prompt_folds_terms_and_clears_when_empty() {
        // With terms: the slot's base prompt is rebuilt to base + vocabulary.
        let sd = SessionDictionary {
            dictionary: Dictionary {
                terms: vec!["Codex".to_owned()],
                replacements: Vec::new(),
            },
            max_terms: 80,
            max_chars: 1200,
            enabled: true,
        };
        let mut slot = Some("base hint".to_owned());
        sd.fold_into_prompt(&mut slot);
        let folded = slot.expect("prompt present");
        assert!(folded.contains("base hint"), "{folded}");
        assert!(folded.contains("Vocabulary: Codex"), "{folded}");

        // A term-less base still folds through initial_prompt: the base
        // prompt survives on its own (no vocabulary line to append).
        let bare = SessionDictionary {
            dictionary: Dictionary::default(),
            max_terms: 80,
            max_chars: 1200,
            enabled: true,
        };
        let mut only_base = Some("keep me".to_owned());
        bare.fold_into_prompt(&mut only_base);
        assert_eq!(only_base.as_deref(), Some("keep me"));

        // Empty base + no terms collapses the slot to None (the caller then
        // passes the empty string through to the endpoint).
        let mut empty = None;
        bare.fold_into_prompt(&mut empty);
        assert_eq!(empty, None);
    }

    #[test]
    fn load_session_dictionary_reads_env_dictionary() {
        // Env-driven load: `VOICEPI_DICTIONARY` + `VOICEPI_DICTIONARY_ENABLED`
        // point at a temp file; the loaded terms + replacements come back on
        // the SessionDictionary. Serialised via the crate-wide ENV_LOCK.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // Snapshot/restore every dictionary var (restore fires on drop), and
        // pin the budgets explicitly rather than inheriting them: an external
        // `VOICEPI_DICTIONARY_MAX_TERMS=0` (or a tiny `_PROMPT_CHARS`) would
        // otherwise drop the vocabulary line and break the prompt assertion.
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.json");
        std::fs::write(
            &path,
            r#"{"terms":["Codex"],"replacements":{"code x":"Codex"}}"#,
        )
        .unwrap();
        std::env::set_var("VOICEPI_DICTIONARY", &path);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        std::env::set_var("VOICEPI_DICTIONARY_MAX_TERMS", "80");
        std::env::set_var("VOICEPI_DICTIONARY_PROMPT_CHARS", "1200");

        let sd = load_session_dictionary();

        assert!(sd.enabled);
        assert!(sd.has_replacements());
        assert_eq!(sd.dictionary.terms, vec!["Codex".to_owned()]);
        let prompt = sd.initial_prompt(None).expect("prompt from terms");
        assert!(prompt.contains("Vocabulary: Codex"), "{prompt}");
    }

    #[test]
    fn reloading_dictionary_picks_up_file_edits() {
        // Live-reload: a ReloadingDictionary re-reads the file at each `current`
        // call and reloads on a freshness/settings miss, so an edit to the
        // dictionary between utterances takes effect -- Python's per-utterance
        // `_dictionary_runtime`. The path is config-driven (the reload resolves
        // config-first), so no env dictionary vars are needed.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        write_dictionary_config(&dir.path().join("config.json"), &dict, true);

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::ConfigFirst);
        let (before, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(before, "hi world");

        // Edit the file to a DIFFERENT byte length so the size component of the
        // freshness stamp flips deterministically (a same-length edit would
        // still be caught by the nanosecond mtime, but size makes the test
        // robust regardless of filesystem mtime granularity).
        std::fs::write(&dict, r#"{"replacements":{"hello":"HELLO"}}"#).unwrap();
        let (after, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(after, "HELLO world");
    }

    #[test]
    fn reloading_dictionary_reflects_enabled_toggle() {
        // Disabling the dictionary in config.json (a Settings save, no restart)
        // flips the cache key's `enabled` field, so the next `current` reloads
        // to an empty (passthrough) table -- the `dictionary_enabled` live
        // setting takes effect without an app restart, resolved config-first.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        let config = dir.path().join("config.json");
        write_dictionary_config(&config, &dict, true);

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::ConfigFirst);
        let (enabled, _) = provider.current().apply_replacements("hello").unwrap();
        assert_eq!(enabled, "hi");

        // Re-save config.json with the dictionary disabled.
        write_dictionary_config(&config, &dict, false);
        let (disabled, _) = provider.current().apply_replacements("hello").unwrap();
        assert_eq!(
            disabled, "hello",
            "disabling the dictionary in config must reach the reload"
        );
    }

    #[test]
    fn reloading_dictionary_env_first_honours_env_path() {
        // The EnvFirst provider (used by the env-driven `simulate-session` verb
        // + the groq-cli smoke) resolves the dictionary from the
        // `VOICEPI_DICTIONARY*` env the worker exports, so an env-set dictionary
        // applies its replacements regardless of config.json.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hey"}}"#).unwrap();
        std::env::set_var("VOICEPI_DICTIONARY", &dict);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        // A config pointing elsewhere must NOT win under EnvFirst.
        std::env::remove_var("VOICEPI_CONFIG");

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::EnvFirst);
        let (out, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(out, "hey world");
    }

    #[test]
    fn reload_resolves_config_first_over_stale_env() {
        // P1 regression: the worker exports `VOICEPI_DICTIONARY_ENABLED` once at
        // startup and a Settings save only rewrites config.json (no restart), so
        // the live-reload must honour config over the stale startup env or a
        // disable/enable in Settings never takes effect. `from_config_and_env`
        // (used by the reload) returns the config value even when env disagrees;
        // `from_env_and_config` (the `dictionary-runtime` RPC path) keeps env
        // precedence.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        // config.json says DISABLED; a stale startup env says ENABLED.
        write_dictionary_config(&dir.path().join("config.json"), &dict, false);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");

        assert!(
            !RuntimeDictionarySettings::from_config_and_env()
                .expect("config readable")
                .enabled,
            "config-first reload must honour the saved (disabled) config over stale env"
        );
        assert!(
            RuntimeDictionarySettings::from_env_and_config().enabled,
            "the RPC path keeps env precedence"
        );
    }

    #[test]
    fn config_first_falls_back_to_env_for_absent_keys() {
        // A partial/legacy config.json that OMITS the dictionary keys must not
        // let the non-empty DEFAULT dictionary path shadow an env-supplied one:
        // absent keys fall back to the process env (then default).
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(&config, "{}").unwrap(); // no dictionary keys at all
        std::env::set_var("VOICEPI_CONFIG", &config);
        std::env::set_var("VOICEPI_DICTIONARY", &dict);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");

        let settings = RuntimeDictionarySettings::from_config_and_env().expect("config readable");
        assert_eq!(
            settings.paths,
            vec![dict],
            "an absent config path must fall back to the env path"
        );
        assert!(settings.enabled, "absent enabled must fall back to env");
    }

    #[test]
    fn config_first_returns_none_when_config_is_unreadable() {
        // A present-but-unparseable config.json (e.g. caught mid Settings save)
        // must resolve to None so the reload keeps its last-good table, rather
        // than `unwrap_or_default()` masking the failure as valid defaults.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(&config, "{ this is not json").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &config);

        assert!(
            RuntimeDictionarySettings::from_config_and_env().is_none(),
            "an unreadable config must yield None (keep last-good), not defaults"
        );
    }

    #[test]
    fn reloading_dictionary_reloads_terms_not_just_replacements() {
        // Term coverage: the reloaded table carries the file's `terms` too, and
        // a file edit updates them (not only the replacement map).
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(
            &dict,
            r#"{"terms":["Codex"],"replacements":{"cloud code":"Claude Code"}}"#,
        )
        .unwrap();
        write_dictionary_config(&dir.path().join("config.json"), &dict, true);

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::ConfigFirst);
        assert_eq!(provider.current().terms, vec!["Codex".to_owned()]);

        // Edit the file (different byte length) -> the reloaded terms update.
        std::fs::write(
            &dict,
            r#"{"terms":["Codex","Slack"],"replacements":{"cloud code":"Claude Code"}}"#,
        )
        .unwrap();
        assert_eq!(
            provider.current().terms,
            vec!["Codex".to_owned(), "Slack".to_owned()],
            "a file edit must reload the term list, not only replacements"
        );
    }

    #[test]
    fn config_dictionary_paths_splits_multi_file_lists() {
        // A `dictionary` config value that is a platform path-separator list
        // (e.g. `a.json;b.json` on Windows) must split into one path per file,
        // matching `env_paths`, not wrap the whole list in one bogus PathBuf.
        let joined =
            std::env::join_paths([PathBuf::from("a.json"), PathBuf::from("b.json")]).unwrap();
        let configured = config::AppSettings {
            dictionary: joined.to_string_lossy().into_owned(),
            ..Default::default()
        };
        assert_eq!(
            config_dictionary_paths(&configured),
            vec![PathBuf::from("a.json"), PathBuf::from("b.json")]
        );
    }

    #[test]
    fn config_first_honours_json_boolean_enabled() {
        // A hand-written config.json with a JSON boolean `false` (not the string
        // "0") must disable the dictionary -- the typed loader's `bool_value`
        // only parses strings, so config-first reads the raw value directly.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let dict = dir.path().join("dict.json");
        std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        let config = dir.path().join("config.json");
        let raw = serde_json::json!({
            "dictionary": dict.display().to_string(),
            "dictionary_enabled": false,
        });
        std::fs::write(&config, serde_json::to_string(&raw).unwrap()).unwrap();
        std::env::set_var("VOICEPI_CONFIG", &config);

        assert!(
            !RuntimeDictionarySettings::from_config_and_env()
                .expect("config readable")
                .enabled,
            "a JSON boolean false must disable the dictionary"
        );
    }

    #[test]
    fn reloading_dictionary_uses_readable_subset_on_partial_failure() {
        // Multiple configured files with one broken: the reload must keep the
        // readable subset's replacements rather than discarding everything, and
        // leave the key unadvanced so the broken file is retried.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _env = DictEnvGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        std::fs::write(&good, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        let joined = std::env::join_paths([&good, &bad]).unwrap();
        std::env::set_var("VOICEPI_DICTIONARY", &joined);
        std::env::set_var("VOICEPI_DICTIONARY_ENABLED", "1");
        std::env::remove_var("VOICEPI_CONFIG");

        let mut provider = ReloadingDictionary::new(ReloadPrecedence::EnvFirst);
        let (out, _) = provider
            .current()
            .apply_replacements("hello world")
            .unwrap();
        assert_eq!(
            out, "hi world",
            "the readable file's replacements must apply despite a broken sibling"
        );
    }

    #[test]
    fn preview_dictionary_reports_counts_and_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(
            &path,
            r#"{"terms":["Codex","Claude Code"],"replacements":{"code X":"Codex"}}"#,
        )
        .unwrap();

        let preview = preview_dictionary(&path, Some("Base prompt"), 10, 1200).unwrap();

        assert_eq!(preview.path, path);
        assert_eq!(preview.term_count, 2);
        assert_eq!(preview.replacement_count, 1);
        assert_eq!(
            preview.prompt.as_deref(),
            Some("Base prompt\nVocabulary: Codex, Claude Code")
        );
    }

    #[test]
    fn runtime_dictionary_applies_prompt_terms_and_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(
            &path,
            r#"{"terms":["Slack","Claude Code","Codex"],"replacements":{"Cloud Code":"Claude Code","code X":"Codex"}}"#,
        )
        .unwrap();
        let settings = RuntimeDictionarySettings::new(true, vec![path.clone()], 10, 1200);

        let result = runtime_dictionary_result(
            &settings,
            Some("Base prompt"),
            "Open Cloud Code and code X.",
        );

        assert!(result.enabled);
        let expected_path = path.display().to_string();
        assert_eq!(result.path.as_deref(), Some(expected_path.as_str()));
        assert_eq!(result.loaded_paths, vec![path.display().to_string()]);
        assert_eq!(result.term_count, 3);
        assert_eq!(result.replacement_count, 2);
        assert_eq!(result.terms, vec!["Slack", "Claude Code", "Codex"]);
        assert_eq!(result.all_terms, vec!["Slack", "Claude Code", "Codex"]);
        assert_eq!(
            result.prompt.as_deref(),
            Some("Base prompt\nVocabulary: Slack, Claude Code, Codex")
        );
        assert_eq!(result.text, "Open Claude Code and Codex.");
        assert_eq!(
            result.changes,
            vec![
                ReplacementChange {
                    from: "Cloud Code".to_owned(),
                    to: "Claude Code".to_owned(),
                    count: 1,
                },
                ReplacementChange {
                    from: "code X".to_owned(),
                    to: "Codex".to_owned(),
                    count: 1,
                },
            ]
        );
        assert_eq!(result.error, None);
    }

    #[test]
    fn runtime_dictionary_disabled_preserves_base_prompt_and_text() {
        let settings =
            RuntimeDictionarySettings::new(false, vec![PathBuf::from("dictionary.json")], 10, 1200);

        let result = runtime_dictionary_result(&settings, Some("Base prompt"), "Cloud Code");

        assert!(!result.enabled);
        assert_eq!(result.prompt.as_deref(), Some("Base prompt"));
        assert_eq!(result.text, "Cloud Code");
        assert!(result.terms.is_empty());
        assert!(result.all_terms.is_empty());
        assert!(result.changes.is_empty());
    }

    #[test]
    fn runtime_dictionary_missing_file_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let settings = RuntimeDictionarySettings::new(true, vec![missing.clone()], 10, 1200);

        let result = runtime_dictionary_result(&settings, Some("Base prompt"), "Cloud Code");

        let expected_path = missing.display().to_string();
        assert_eq!(result.path.as_deref(), Some(expected_path.as_str()));
        assert!(result.loaded_paths.is_empty());
        assert_eq!(result.prompt.as_deref(), Some("Base prompt"));
        assert_eq!(result.text, "Cloud Code");
        assert_eq!(result.error, None);
    }

    #[test]
    fn runtime_dictionary_reports_parse_errors_without_rewriting_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        std::fs::write(&path, "{not json").unwrap();
        let settings = RuntimeDictionarySettings::new(true, vec![path], 10, 1200);

        let result = runtime_dictionary_result(&settings, Some("Base prompt"), "Cloud Code");

        assert_eq!(result.prompt.as_deref(), Some("Base prompt"));
        assert_eq!(result.text, "Cloud Code");
        assert!(result.error.unwrap().contains("dictionary.json"));
    }

    #[test]
    fn runtime_dictionary_merges_paths_and_later_replacements_win() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.txt");
        std::fs::write(
            &first,
            r#"{"terms":["Codex"],"replacements":{"code X":"wrong"}}"#,
        )
        .unwrap();
        std::fs::write(
            &second,
            "terms:\n- Claude Code\nreplacements:\ncode X => Codex\n",
        )
        .unwrap();
        let settings = RuntimeDictionarySettings::new(true, vec![first, second], 10, 1200);

        let result = runtime_dictionary_result(&settings, None, "try code X");

        assert_eq!(result.terms, vec!["Codex", "Claude Code"]);
        assert_eq!(result.text, "try Codex");
        assert_eq!(
            result.replacements,
            vec![Replacement {
                from: "code X".to_owned(),
                to: "Codex".to_owned(),
            }]
        );
    }
}
