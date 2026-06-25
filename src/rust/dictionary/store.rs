//! Disk IO for the dictionary file plus path resolution + sanitisation.
//!
//! The Rust port of `vp_dictionary_store.py` (Wave 4-A of the Python-removal
//! roadmap #348). Three responsibilities:
//!
//! 1. **Path resolution** — turn `Option<&str>` + `VOICEPI_DICTIONARY` env +
//!    the per-user default into one canonical [`PathBuf`].
//! 2. **Sanitisation** — defensive normalisation of a user-chosen path
//!    (`~` expansion, `.json` enforcement, parent-traversal rejection).
//! 3. **Read / write** — load the document, mutate, write back while
//!    preserving every other top-level key (notably `replacements`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

use super::parse::{
    parse_dictionary_replacements, parse_dictionary_terms, parse_json_dictionary,
    parse_mapping_line,
};
use super::{dedupe_terms, env_paths, parse_dictionary, Dictionary, Replacement};

/// Load + parse a dictionary file (JSON or plain-text) from disk.
pub fn load_dictionary(path: impl AsRef<Path>) -> Result<Dictionary> {
    let raw = fs::read_to_string(path)?;
    parse_dictionary(&raw)
}

/// Make sure `path` exists as a JSON dictionary (creating an empty one if not).
/// Returns the same path so the caller can chain it.
pub fn ensure_json_dictionary(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if !path.exists() {
        path.parent().map(fs::create_dir_all).transpose()?;
        write_json_dictionary(path, &Dictionary::default(), None)?;
    }
    Ok(path.to_path_buf())
}

/// Add a single vocabulary term. Returns `true` when the term was new (and the
/// file was rewritten), `false` when it was already present case-insensitively.
pub fn add_term(path: impl AsRef<Path>, term: &str) -> Result<bool> {
    let term = term.trim();
    if term.is_empty() {
        return Err(anyhow!("dictionary term cannot be empty"));
    }
    let path = path.as_ref();
    let (mut dictionary, base) = load_json_dictionary_for_write(path)?;
    let before = dictionary.terms.len();
    dictionary.terms = dedupe_terms(
        dictionary
            .terms
            .into_iter()
            .chain(std::iter::once(term.to_owned()))
            .collect(),
    );
    let added = dictionary.terms.len() != before;
    if added {
        write_json_dictionary(path, &dictionary, Some(base))?;
    }
    Ok(added)
}

/// Add or update a deterministic replacement (`FROM=TO` form). Returns the
/// parsed `(from, to)` and a `changed` flag.
pub fn add_replacement(path: impl AsRef<Path>, mapping: &str) -> Result<(String, String, bool)> {
    let (from, to) =
        parse_mapping_line(mapping).ok_or_else(|| anyhow!("replacement must be FROM=TO"))?;
    let path = path.as_ref();
    let (mut dictionary, base) = load_json_dictionary_for_write(path)?;
    let changed = dictionary
        .replacements
        .iter()
        .find(|replacement| replacement.from == from)
        .is_none_or(|replacement| replacement.to != to);
    if changed {
        dictionary
            .replacements
            .retain(|replacement| replacement.from != from);
        dictionary.replacements.push(Replacement {
            from: from.clone(),
            to: to.clone(),
        });
        write_json_dictionary(path, &dictionary, Some(base))?;
    }
    Ok((from, to, changed))
}

/// Per-user default dictionary path. Mirrors the Python helper exactly:
/// `%APPDATA%/WhisperDictate/dictionary.json` on Windows,
/// `$XDG_CONFIG_HOME/whisper-dictate/dictionary.json` elsewhere.
pub fn default_dictionary_path() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = dirs_home();
                home.join("AppData").join("Roaming")
            });
        base.join("WhisperDictate").join("dictionary.json")
    } else {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs_home().join(".config"));
        base.join("whisper-dictate").join("dictionary.json")
    }
}

/// Resolve the dictionary path: explicit arg wins, then `VOICEPI_DICTIONARY`
/// (first non-empty entry of an `os.pathsep` list), then the per-user default.
///
/// An EXPLICIT empty/blank `path` (e.g. `--dictionary ""`) is rejected with a
/// clear error — silently swapping in a different file would be surprising.
/// Pass `None` to opt into the env / default fall-through.
pub fn resolve_dictionary_path(path: Option<&str>) -> Result<PathBuf> {
    if let Some(value) = path {
        if value.trim().is_empty() {
            return Err(anyhow!(
                "dictionary path is empty; omit --dictionary to use the default location instead of passing an empty value"
            ));
        }
        return Ok(PathBuf::from(value));
    }
    if let Some(paths) = env_paths("VOICEPI_DICTIONARY") {
        if let Some(first) = paths.into_iter().next() {
            return Ok(first);
        }
    }
    Ok(default_dictionary_path())
}

/// Defensively normalise a user-chosen dictionary path before any IO:
///
/// * expand `~`, then canonicalise to an absolute path;
/// * require a `.json` suffix so a typo'd `~/.bashrc` is rejected;
/// * reject literal `..` segments in the *raw* input (before resolution).
///
/// Mirrors `vp_dictionary_store.sanitize_dictionary_path` exactly.
pub fn sanitize_dictionary_path(path: &str) -> Result<PathBuf> {
    let raw = path.trim();
    if raw.is_empty() {
        return Err(anyhow!("dictionary path must not be empty"));
    }
    let raw_path = PathBuf::from(raw);
    if raw_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(anyhow!("dictionary path must not contain '..': {raw:?}"));
    }
    let expanded = expand_user(raw);
    // Path::canonicalize requires the file to exist; mimic Python's
    // `Path.resolve(strict=False)` by canonicalising the parent we can find
    // and re-joining the rest. For the common case (parent exists), this
    // collapses `.` / `..` correctly.
    let normalised = normalise_path(&expanded);
    if normalised
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
        != Some("json")
    {
        return Err(anyhow!("dictionary path must end in .json: {raw:?}"));
    }
    Ok(normalised)
}

/// Read the dictionary document (top-level JSON object), `{}` when absent.
pub fn load_dictionary_document(path: &Path) -> Result<Map<String, Value>> {
    let sanitized = sanitize_dictionary_path(path.to_string_lossy().as_ref())?;
    if !sanitized.exists() {
        return Ok(Map::new());
    }
    let raw = fs::read_to_string(&sanitized)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|err| {
        anyhow!(
            "dictionary at {} is not valid JSON: {err}",
            sanitized.display()
        )
    })?;
    value.as_object().cloned().ok_or_else(|| {
        anyhow!(
            "dictionary at {} root must be a JSON object",
            sanitized.display()
        )
    })
}

/// Extract the term strings from an already-loaded dictionary document.
pub fn terms_from_document(document: &Map<String, Value>) -> Vec<String> {
    parse_dictionary_terms(document)
        .into_iter()
        .filter_map(|term| {
            let trimmed = term.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        })
        .collect()
}

/// Write `terms` into the dictionary file, preserving every other top-level
/// key. Mirrors `vp_dictionary_store.write_terms` (used by the training CLI to
/// append corpus-mined terms without dropping the user's replacements).
pub fn write_terms(
    path: &Path,
    terms: &[String],
    base: Option<Map<String, Value>>,
) -> Result<PathBuf> {
    let sanitized = sanitize_dictionary_path(path.to_string_lossy().as_ref())?;
    let mut document: BTreeMap<String, Value> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    if let Some(base) = base {
        for (key, value) in base {
            if !document.contains_key(&key) {
                order.push(key.clone());
            }
            document.insert(key, value);
        }
    }
    if !document.contains_key("terms") {
        order.push("terms".to_owned());
    }
    document.insert(
        "terms".to_owned(),
        Value::Array(terms.iter().cloned().map(Value::String).collect()),
    );
    if !document.contains_key("replacements") {
        order.push("replacements".to_owned());
        document.insert("replacements".to_owned(), Value::Object(Map::new()));
    }
    let mut object = Map::new();
    for key in order {
        if let Some(value) = document.remove(&key) {
            object.insert(key, value);
        }
    }
    if let Some(parent) = sanitized.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &sanitized,
        serde_json::to_string_pretty(&Value::Object(object))? + "\n",
    )?;
    Ok(sanitized)
}

pub(crate) fn load_json_dictionary_for_write(
    path: &Path,
) -> Result<(Dictionary, Map<String, Value>)> {
    ensure_json_dictionary(path)?;
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let base = value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("dictionary JSON root must be an object"))?;
    let dictionary = parse_json_dictionary(&raw)?;
    Ok((dictionary, base))
}

pub(crate) fn write_json_dictionary(
    path: &Path,
    dictionary: &Dictionary,
    base: Option<Map<String, Value>>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut object = base.unwrap_or_default();
    object.insert(
        "terms".to_owned(),
        Value::Array(
            dedupe_terms(dictionary.terms.clone())
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );
    let mut replacements = Map::new();
    for replacement in sorted_replacements(&dictionary.replacements) {
        replacements.insert(
            replacement.from.clone(),
            Value::String(replacement.to.clone()),
        );
    }
    object.insert("replacements".to_owned(), Value::Object(replacements));
    let _ = parse_dictionary_replacements; // keep symbol referenced for future-proofing
    fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(object))? + "\n",
    )?;
    Ok(())
}

fn sorted_replacements(replacements: &[Replacement]) -> Vec<Replacement> {
    let mut out = replacements.to_vec();
    out.sort_by(|a, b| a.from.cmp(&b.from));
    out
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn expand_user(raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix('~') {
        let home = dirs_home();
        let rest = stripped.trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            return home;
        }
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Best-effort normalisation: walk components collapsing `.` and `..` without
/// requiring the file to exist. Absolutises against the current dir when the
/// path is relative. Mirrors Python's `Path.expanduser().resolve()` enough for
/// the suffix + traversal checks we make in [`sanitize_dictionary_path`].
fn normalise_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_term_creates_json_dictionary_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");

        assert!(add_term(&path, "Codex").unwrap());
        assert!(!add_term(&path, "codex").unwrap());

        let dictionary = load_dictionary(&path).unwrap();
        assert_eq!(dictionary.terms, vec!["Codex"]);
    }

    #[test]
    fn add_replacement_preserves_unknown_json_fields_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        fs::write(
            &path,
            r#"{"terms":["Codex"],"notes":"keep","replacements":{"z":"Z"}}"#,
        )
        .unwrap();

        let (from, to, changed) = add_replacement(&path, "a=A").unwrap();

        assert_eq!((from.as_str(), to.as_str(), changed), ("a", "A", true));
        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["notes"], "keep");
        let keys = value["replacements"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["a", "z"]);
    }

    #[test]
    fn sanitize_rejects_non_json_suffix() {
        assert!(sanitize_dictionary_path("/tmp/.bashrc").is_err());
    }

    #[test]
    fn sanitize_rejects_parent_traversal() {
        assert!(sanitize_dictionary_path("../../etc/evil.json").is_err());
    }

    #[test]
    fn sanitize_rejects_blank() {
        assert!(sanitize_dictionary_path("   ").is_err());
    }

    #[test]
    fn sanitize_expands_user_home() {
        let p = sanitize_dictionary_path("~/whisper-dictate-dict.json").unwrap();
        assert!(!p.to_string_lossy().contains('~'));
        assert!(p.is_absolute());
    }

    #[test]
    fn resolve_dictionary_path_explicit_wins() {
        let path = resolve_dictionary_path(Some("/tmp/custom.json")).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/custom.json"));
    }

    #[test]
    fn resolve_dictionary_path_rejects_explicit_empty() {
        for empty in ["", "  "] {
            assert!(resolve_dictionary_path(Some(empty)).is_err());
        }
    }

    #[test]
    fn write_terms_preserves_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        fs::write(
            &path,
            r#"{"terms":["Old"],"replacements":{"Cloud Code":"Claude Code"},"notes":"keep me"}"#,
        )
        .unwrap();
        let document = load_dictionary_document(&path).unwrap();
        write_terms(&path, &["Old".to_owned(), "New".to_owned()], Some(document)).unwrap();
        let reloaded: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reloaded["terms"], serde_json::json!(["Old", "New"]));
        assert_eq!(reloaded["replacements"]["Cloud Code"], "Claude Code");
        assert_eq!(reloaded["notes"], "keep me");
    }

    #[test]
    fn write_terms_creates_parent_dirs_and_default_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("dir")
            .join("dictionary.json");
        write_terms(&path, &["A".to_owned()], None).unwrap();
        assert!(path.exists());
        let reloaded: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reloaded["terms"], serde_json::json!(["A"]));
        assert_eq!(reloaded["replacements"], serde_json::json!({}));
    }

    #[test]
    fn load_dictionary_document_missing_yields_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        assert!(load_dictionary_document(&path).unwrap().is_empty());
    }

    #[test]
    fn load_dictionary_document_rejects_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        fs::write(&path, "{not json").unwrap();
        assert!(load_dictionary_document(&path).is_err());
    }

    #[test]
    fn load_dictionary_document_rejects_non_object_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        fs::write(&path, "[1,2,3]").unwrap();
        assert!(load_dictionary_document(&path).is_err());
    }

    #[test]
    fn terms_from_document_handles_string_and_object_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");
        fs::write(
            &path,
            r#"{"terms":["Slack",{"term":"Claude Code"},"","  "],"replacements":{}}"#,
        )
        .unwrap();
        let doc = load_dictionary_document(&path).unwrap();
        assert_eq!(terms_from_document(&doc), vec!["Slack", "Claude Code"]);
    }
}
