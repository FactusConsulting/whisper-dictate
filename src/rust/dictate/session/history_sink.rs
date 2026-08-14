//! History-JSONL sink for [`super::DictateSession`].
//!
//! Ports the WRITE side of `vp_history.py` to Rust so an in-process Rust
//! engine session records every completed utterance to the same local
//! JSONL file the Python engine writes to today (parity blocker #1 for
//! flipping the default engine to Rust — the READ side already lives in
//! [`crate::history`] and honours the same [`crate::telemetry::history_path_from_settings`]
//! path, so writer + reader always agree).
//!
//! # Contract
//!
//! * **Path**: [`crate::telemetry::history_path_from_settings`] --
//!   `settings.history_jsonl` when set (mirrors Python's
//!   `VOICEPI_HISTORY_JSONL` config setting), else the platform default
//!   from [`crate::config::default_history_path`]:
//!   - Windows: `%APPDATA%\WhisperDictate\history.jsonl`
//!   - Linux/macOS: `$XDG_STATE_HOME/whisper-dictate/history.jsonl`
//!     (default `~/.local/state`)
//!
//!   These are the exact same defaults `vp_history.default_history_path`
//!   returns, so a user upgrading from the Python engine keeps appending
//!   to their existing file with no schema break.
//!
//! * **Gate**: `settings.history_enabled` (default `true`, mirrors
//!   Python's `VOICEPI_HISTORY_ENABLED` config setting). When disabled,
//!   [`history_sink_from_settings`] returns `None` so no sink is even
//!   constructed -- the session pays zero per-utterance cost.
//!
//! * **Schema**: [`crate::telemetry::history_event`] filters the utterance
//!   event down to the exact same `HISTORY_KEYS` allow-list Python's
//!   `_history_event` uses, so a Rust-written row is byte-identical to a
//!   Python-written row for the same input event. [`crate::telemetry::append_jsonl`]
//!   handles the file write itself (create parent dirs, open with append,
//!   compact JSON + newline).
//!
//! * **Errors**: non-fatal. A failed write logs one `[history]` warning to
//!   stderr and the session continues -- matching Python's
//!   `_record_utterance_event`, which wraps `append_record_sinks` in
//!   `try / except OSError`. A history-file break can never abort a
//!   dictation.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::Value;

use super::path_util::expand_user;
use crate::config;
use crate::telemetry;

/// Config-key + env-var pair for the "history enabled" gate. Kept as
/// named constants because the same string pair is consulted from both
/// [`effective_history_settings`] and the tests, and the Python worker's
/// `vp_history.history_enabled` reads the same names via `get_value`.
const HISTORY_ENABLED_KEY: &str = "history_enabled";
const HISTORY_ENABLED_ENV: &str = "VOICEPI_HISTORY_ENABLED";
/// Config-key + env-var pair for the "history JSONL path" override.
const HISTORY_JSONL_KEY: &str = "history_jsonl";
const HISTORY_JSONL_ENV: &str = "VOICEPI_HISTORY_JSONL";

/// Resolved history settings AFTER config→env→default overlay. Returned
/// by [`effective_history_settings`] and consumed by both
/// [`history_sink_from_settings`] and [`ReloadingHistorySink::append`]
/// so the two seams cannot drift on the precedence rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveHistorySettings {
    /// Whether the history file should be written. Mirrors Python's
    /// `vp_history.history_enabled()` result.
    pub enabled: bool,
    /// Absolute path the history JSONL is written to. Always populated
    /// (`config::default_history_path` on the "unset" branch) so the
    /// caller does not have to re-derive the default. Any leading `~`
    /// in an env / config override is expanded via
    /// [`super::path_util::expand_user`] to match Python's
    /// `os.path.expanduser`, so `~/.voicepi/history.jsonl` writes to
    /// `$HOME/.voicepi/history.jsonl` rather than a literal `~`
    /// directory under `cwd` (Codex P2 #620 history_sink.rs:107).
    pub path: PathBuf,
    /// `Some(err)` when `config.json` could NOT be loaded (I/O error or
    /// JSON parse failure). Codex P2 #620 finding
    /// `Fail closed when live history config cannot be read`: previously
    /// this error was swallowed via `load_raw_config().unwrap_or(Null)`,
    /// so a user whose config says `history_enabled=false` could have
    /// dictation persisted while the file was being rewritten or was
    /// temporarily invalid. [`ReloadingHistorySink::append`] now checks
    /// this field and skips the append with a `[history]` warn line.
    pub config_error: Option<String>,
}

/// Resolve the effective history settings the way Python's
/// `vp_history` does: **config-file value wins first, then env var,
/// then schema default**. Mirrors `vp_config.get_value` (which is what
/// `vp_history.history_enabled()` / `history_path()` both call). The
/// Rust [`config::AppSettings`] path only reads config.json, so left
/// alone it silently ignores `VOICEPI_HISTORY_ENABLED=0` /
/// `VOICEPI_HISTORY_JSONL=...` set in the environment -- this helper is
/// the sinks' single overlay point. Codex P1 #605 finding 1.
pub fn effective_history_settings() -> EffectiveHistorySettings {
    // Config file first (the "user saved a value in the UI" path).
    // Preserve the load error rather than collapsing it to `Null` --
    // the `ReloadingHistorySink` inspects `config_error` to fail-closed
    // on a transient/malformed config.json (Codex P2 #620 finding
    // `Fail closed when live history config cannot be read`).
    let (raw_config, config_error) = match config::load_raw_config() {
        Ok(v) => (v, None),
        Err(err) => (serde_json::Value::Null, Some(err.to_string())),
    };
    let object = raw_config.as_object();

    let enabled_from_config = object
        .and_then(|obj| obj.get(HISTORY_ENABLED_KEY))
        .and_then(value_as_env_string);
    let path_from_config = object
        .and_then(|obj| obj.get(HISTORY_JSONL_KEY))
        .and_then(value_as_env_string);

    // Config → env → schema default (`"1"` for enabled).
    let enabled_raw = enabled_from_config
        .or_else(|| std::env::var(HISTORY_ENABLED_ENV).ok())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "1".to_owned());
    let enabled = is_truthy(&enabled_raw);

    let path_raw = path_from_config
        .or_else(|| std::env::var(HISTORY_JSONL_ENV).ok())
        .filter(|v| !v.trim().is_empty());
    let path = match path_raw {
        // `~/.voicepi/history.jsonl` must land in $HOME, not a literal
        // `~` directory under `cwd`. Python's `history_path()` calls
        // `expanduser()`; without this the Rust writer would silently
        // diverge from the Python reader / the metrics sink (which
        // already expanded via `expand_user`). Codex P2 #620
        // history_sink.rs:107.
        Some(raw) => expand_user(raw.trim()),
        None => config::default_history_path(),
    };

    EffectiveHistorySettings {
        enabled,
        path,
        config_error,
    }
}

/// Mirror of Python's `_truthy` in `vp_history.py`: everything except
/// the falsy tokens is truthy, including the empty default `"1"`.
fn is_truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// Normalise a JSON value the same way `config::schema::value_to_env_string`
/// does, so a `bool` (`true`/`false` from config.json) or a string are
/// both accepted as a truthy/falsy history-enabled value. `null` /
/// empty-string are treated as "unset" (fall through to the env layer).
fn value_as_env_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(v) if v.is_empty() => None,
        serde_json::Value::String(v) => Some(v.clone()),
        serde_json::Value::Bool(true) => Some("1".to_owned()),
        serde_json::Value::Bool(false) => Some("0".to_owned()),
        other => Some(other.to_string()),
    }
}

/// The seam the session calls on every successful utterance. The
/// production impl [`JsonlHistorySink`] writes to the local JSONL file;
/// [`NoopHistorySink`] exists for tests that want the sink attached
/// without touching disk.
pub trait HistorySink {
    /// Record one utterance event. `event` is the payload the session
    /// just emitted on the worker-event stream (already includes `ts`,
    /// `text`, `stt_backend`-family fields the wire layer populates); the
    /// implementation is responsible for the Python-parity field filter
    /// via [`crate::telemetry::history_event`] before writing.
    fn append(&self, event: &Value);
}

/// Writes each utterance to a JSONL file on disk after filtering the event
/// down to the [`crate::telemetry::history_event`] allow-list. Path is
/// resolved at construction and cached; the process env / settings are
/// re-read at construction, not per utterance, so a Settings save requires
/// building a fresh session (matching the current supervisor lifecycle).
pub struct JsonlHistorySink {
    path: PathBuf,
}

impl JsonlHistorySink {
    /// Build a sink that writes to `path`. Prefer [`history_sink_from_settings`]
    /// in production so the gate + path resolution match Python.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The file this sink writes to. Exposed so tests can assert
    /// [`history_sink_from_settings`] picked the expected path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl HistorySink for JsonlHistorySink {
    fn append(&self, event: &Value) {
        let filtered = telemetry::history_event(event);
        if let Err(err) = telemetry::append_jsonl(&self.path, &filtered) {
            // Non-fatal, matching Python's
            // `except OSError: print(f"[sinks] could not write ...")`.
            // The prefix is `[history]` (vs Python's `[sinks]`) because
            // this Rust path only writes the history file (metrics.jsonl
            // stays on the Python engine's `append_record_sinks` path for
            // now); tag it distinctly so log-scrapers can tell the two
            // apart during the migration.
            crate::diag::log!(
                "[history] could not append to {}: {err}",
                self.path.display()
            );
        }
    }
}

/// Discard every event. Useful for tests that want to build a session
/// with the sink seam wired up without touching disk, and for callers
/// that want to explicitly disable history for a single session without
/// changing the global gate.
#[derive(Default)]
pub struct NoopHistorySink;

impl HistorySink for NoopHistorySink {
    fn append(&self, _event: &Value) {}
}

/// Live-reloading history sink: re-reads config + env on EVERY
/// [`Self::append`], so a Settings save between utterances (via the
/// UI's `save_settings`) takes effect on the next utterance without an
/// app restart. Mirrors Python's `vp_history._append_history` /
/// `history_enabled()` pair, both of which read the config+env on every
/// call. Codex P1 #605 finding 2.
///
/// The wrapper is cheap: reading `config.json` on each utterance is a
/// small JSON parse, and the sink can decide to skip entirely when the
/// gate has been flipped to disabled. When the path changes between
/// utterances the sink opens the new file on the next `append` (the
/// underlying `telemetry::append_jsonl` reopens per call already).
pub struct ReloadingHistorySink {
    /// Cache of the last-resolved settings so a test can inspect what
    /// the sink saw on its most recent call. Also lets us short-circuit
    /// the "gate flipped off between utterances" path without keeping a
    /// second copy of the [`EffectiveHistorySettings`] around.
    last: Mutex<Option<EffectiveHistorySettings>>,
}

impl ReloadingHistorySink {
    /// Build a reloading sink. No settings are read until the first
    /// [`Self::append`] call, so a session that never fires an
    /// utterance pays no per-construct disk cost either.
    pub fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    /// Last-resolved settings (test hook). `None` before the first
    /// `append` fires.
    #[cfg(test)]
    pub fn last_resolved(&self) -> Option<EffectiveHistorySettings> {
        self.last.lock().ok().and_then(|guard| guard.clone())
    }

    /// Same as [`HistorySink::append`] but returns the underlying write
    /// result instead of swallowing it. Used by the `self-test
    /// history-write` verb (Codex P2 #621 history_write.rs:174) so a
    /// broken file surfaces as `ok=false` in the JSON envelope instead
    /// of the pre-fix hard-coded `Ok(())`. Both the gate-off branch
    /// and the config-error fail-closed branch return `Ok(None)`; a
    /// real I/O error bubbles as an `Err`; a successful append returns
    /// `Ok(Some(path))` so the caller can `metadata()` the exact file
    /// the sink wrote to.
    pub fn append_with_result(&self, event: &Value) -> anyhow::Result<Option<PathBuf>> {
        let settings = effective_history_settings();
        if let Ok(mut guard) = self.last.lock() {
            *guard = Some(settings.clone());
        }
        if let Some(err) = &settings.config_error {
            // Fail-closed: an operator with `history_enabled=false` in
            // a transiently-invalid config.json must NOT have their
            // dictation persisted to the platform default. Log at warn
            // level so the operator sees the broken config in stderr.
            crate::diag::log!(
                "[history] warn: config read failed ({err}); \
                 dropping this row to fail-closed rather than falling through \
                 to defaults -- fix config.json to resume writing"
            );
            return Ok(None);
        }
        if !settings.enabled {
            return Ok(None);
        }
        let filtered = telemetry::history_event(event);
        telemetry::append_jsonl(&settings.path, &filtered)?;
        Ok(Some(settings.path))
    }
}

impl Default for ReloadingHistorySink {
    fn default() -> Self {
        Self::new()
    }
}

impl HistorySink for ReloadingHistorySink {
    fn append(&self, event: &Value) {
        // Delegate to the result-returning variant so the shipping
        // trait impl (which swallows errors) and the self-test verb
        // (which surfaces them, Codex P2 #621 history_write.rs:174)
        // share exactly the same resolve-then-write path. Only the
        // trailing error-handling differs.
        match self.append_with_result(event) {
            Ok(_) => {}
            Err(err) => {
                // Log the path we would have written to (cached from
                // the resolve inside `append_with_result`).
                let path_hint = self
                    .last
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .map(|s| s.path.display().to_string())
                    .unwrap_or_else(|| "<unresolved>".to_owned());
                crate::diag::log!("[history] could not append to {path_hint}: {err}");
            }
        }
    }
}

/// Resolve the production history sink for the session. Always returns
/// a [`ReloadingHistorySink`] -- the sink itself re-reads config + env
/// on every [`ReloadingHistorySink::append`] so a Settings save between
/// utterances takes effect on the next one (Python parity:
/// `_append_history` re-reads `history_enabled()` / `history_path()`
/// per call). Codex P1 #605 findings 1 + 2.
///
/// Callers do not need to pre-check the enabled gate here anymore --
/// the reloading sink short-circuits internally when the gate is off,
/// so a session that always attaches this sink still pays zero write
/// cost when the user has disabled history. Returned as `Option` for
/// API compatibility with the previous "off => None" behaviour; the
/// only caller today (`rust_session_real_backends`) always unwraps to
/// `with_optional_history_sink`.
pub fn history_sink_from_settings() -> Option<Box<dyn HistorySink + Send>> {
    Some(Box::new(ReloadingHistorySink::new()))
}

/// Build the non-live history sink from an already-resolved native snapshot.
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) fn history_sink_from_app_settings(
    settings: &crate::config::AppSettings,
) -> Option<Box<dyn HistorySink + Send>> {
    if !settings.history_enabled {
        return None;
    }
    let path = if settings.history_jsonl.trim().is_empty() {
        config::default_history_path()
    } else {
        expand_user(settings.history_jsonl.trim())
    };
    Some(Box::new(JsonlHistorySink::new(path)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn jsonl_sink_appends_filtered_row_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());

        // Emit an event carrying BOTH allow-listed and non-allow-listed
        // fields; the sink must drop the non-allow-listed ones (parity
        // with Python's `_history_event` filter).
        sink.append(&json!({
            "ts": 1706280000.5,
            "event": "utterance",
            "text": "hello world",
            "stt_backend": "whisper",
            "large_unused_blob": "drop me",
        }));

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.ends_with('\n'), "JSONL row must end with newline");
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(row["text"], "hello world");
        assert_eq!(row["stt_backend"], "whisper");
        assert_eq!(row["ts"], 1706280000.5);
        assert!(
            row.get("large_unused_blob").is_none(),
            "non-allow-listed field must be filtered out"
        );
    }

    #[test]
    fn jsonl_sink_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Path with a not-yet-created parent -- the sink must create it,
        // matching the existing `telemetry::append_jsonl` contract.
        let path = dir.path().join("nested").join("dirs").join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());
        sink.append(&json!({"text": "hi"}));
        assert!(
            path.exists(),
            "sink must create parent directories on first write"
        );
    }

    #[test]
    fn jsonl_sink_appends_multiple_rows_preserving_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());

        sink.append(&json!({"text": "first"}));
        sink.append(&json!({"text": "second"}));
        sink.append(&json!({"text": "third"}));

        let raw = fs::read_to_string(&path).unwrap();
        let texts: Vec<String> = raw
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["text"].to_string())
            .collect();
        assert_eq!(
            texts,
            vec![
                "\"first\"".to_string(),
                "\"second\"".to_string(),
                "\"third\"".to_string()
            ]
        );
    }

    #[test]
    fn jsonl_sink_write_failure_is_non_fatal() {
        // Aim the sink at a path whose parent CANNOT be created (an existing
        // regular file, so `create_dir_all` would fail). The sink must swallow
        // the error -- a dropped dictation would be worse than a missing
        // history row.
        let dir = tempfile::tempdir().unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        fs::write(&file_as_parent, "").unwrap();
        let path = file_as_parent.join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());
        // Must not panic / propagate.
        sink.append(&json!({"text": "should not blow up"}));
        assert!(
            !path.exists(),
            "the unwritable path should stay non-existent -- \
             the sink swallowed the error"
        );
    }

    #[test]
    fn round_trip_write_then_read_via_history_reader() {
        // The whole point of writing from Rust is that the pre-existing
        // `crate::history` reader sees the row. This exercises the
        // WRITER + READER together.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());

        sink.append(&json!({
            "ts": 1706280000.5,
            "text": "hello from rust",
            "stt_backend": "whisper",
        }));

        let rows = crate::history::read_rows(&path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["text"], "hello from rust");
        assert_eq!(rows[0]["stt_backend"], "whisper");

        let last = crate::history::last_row(&path).unwrap().unwrap();
        assert_eq!(last["text"], "hello from rust");
    }

    #[test]
    fn matches_python_row_shape_for_the_same_event() {
        // Parity fixture: given a Python-shaped utterance event, the Rust
        // writer must produce the SAME JSONL row as Python's
        // `append_record_sinks` path (which itself already goes through the
        // shared `telemetry::history_event` filter today, since the Python
        // helper shells out to the same Rust code). This test pins the
        // schema contract so a future refactor that touches EITHER filter
        // catches drift here.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let sink = JsonlHistorySink::new(path.clone());

        let event = json!({
            "ts": 1706280000.5,
            "event": "utterance",
            "text": "hej verden",
            "raw_text": "hej verden",
            "text_preview": "hej verden",
            "text_chars": 10,
            "recording_s": 1.23,
            "audio_duration_s": 1.2,
            "compute_s": 0.04,
            "language": "da",
            "stt_backend": "whisper",
            "model": "large-v3-turbo",
            // Non-allow-listed noise that MUST be filtered out:
            "api_key": "secret",
            "raw_pcm_bytes": 12345,
        });
        sink.append(&event);

        let raw = fs::read_to_string(&path).unwrap();
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        // Every allow-listed field survives with its value.
        for (key, want) in [
            ("ts", &Value::from(1706280000.5)),
            ("event", &Value::from("utterance")),
            ("text", &Value::from("hej verden")),
            ("raw_text", &Value::from("hej verden")),
            ("text_preview", &Value::from("hej verden")),
            ("text_chars", &Value::from(10)),
            ("recording_s", &Value::from(1.23)),
            ("audio_duration_s", &Value::from(1.2)),
            ("compute_s", &Value::from(0.04)),
            ("language", &Value::from("da")),
            ("stt_backend", &Value::from("whisper")),
            ("model", &Value::from("large-v3-turbo")),
        ] {
            assert_eq!(&row[key], want, "field {key} must match");
        }
        // Non-allow-listed fields are gone.
        assert!(row.get("api_key").is_none(), "api_key must be filtered");
        assert!(
            row.get("raw_pcm_bytes").is_none(),
            "raw_pcm_bytes must be filtered"
        );
    }

    #[test]
    fn noop_sink_writes_nothing() {
        // Trivially: the noop sink is a discard, so nothing observable
        // happens. Kept as an executable spec so a future refactor cannot
        // accidentally turn Noop into a fallback that touches disk.
        let sink = NoopHistorySink;
        sink.append(&json!({"text": "ignored"}));
    }

    // ── env overlay + live reload (Codex P1 #605) ──────────────────

    /// RAII snapshot for a set of process-env keys; restores each on
    /// drop. Bundled here to keep the reload tests small.
    struct EnvSnapshot {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }
    impl EnvSnapshot {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|k| (*k, std::env::var_os(k))).collect();
            Self { saved }
        }
    }
    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// Config file wins over env var. Mirrors Python's
    /// `vp_config.get_value`: the config-file value is the highest-priority
    /// source, then env, then default. Fixture:
    ///   config -> history_jsonl="config/history.jsonl"
    ///   env    -> VOICEPI_HISTORY_JSONL="env/history.jsonl"
    ///   want   -> config path (config wins)
    #[test]
    fn effective_history_settings_config_wins_over_env() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", HISTORY_ENABLED_ENV, HISTORY_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(
            &cfg,
            serde_json::json!({
                "history_enabled": "0",
                "history_jsonl": dir.path().join("config-history.jsonl").to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::set_var(HISTORY_ENABLED_ENV, "1");
        std::env::set_var(HISTORY_JSONL_ENV, dir.path().join("env-history.jsonl"));

        let resolved = effective_history_settings();
        assert!(
            !resolved.enabled,
            "config-file `history_enabled=0` must override env-var `=1`"
        );
        assert_eq!(
            resolved.path,
            dir.path().join("config-history.jsonl"),
            "config-file path must override env path"
        );
    }

    /// Env var wins over the schema default. Fixture:
    ///   config -> (no history keys)
    ///   env    -> VOICEPI_HISTORY_ENABLED=0, VOICEPI_HISTORY_JSONL=/env/path
    ///   want   -> disabled + env path (env wins over default)
    #[test]
    fn effective_history_settings_env_wins_over_default() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", HISTORY_ENABLED_ENV, HISTORY_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::set_var(HISTORY_ENABLED_ENV, "0");
        let env_path = dir.path().join("env-only.jsonl");
        std::env::set_var(HISTORY_JSONL_ENV, &env_path);

        let resolved = effective_history_settings();
        assert!(
            !resolved.enabled,
            "env-var `VOICEPI_HISTORY_ENABLED=0` must beat the schema default (`1`)"
        );
        assert_eq!(
            resolved.path, env_path,
            "env-var `VOICEPI_HISTORY_JSONL` must beat the platform default"
        );
    }

    /// Default (nothing set anywhere): enabled=true, path=platform default.
    #[test]
    fn effective_history_settings_all_unset_yields_defaults() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", HISTORY_ENABLED_ENV, HISTORY_JSONL_ENV]);
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::remove_var(HISTORY_ENABLED_ENV);
        std::env::remove_var(HISTORY_JSONL_ENV);

        let resolved = effective_history_settings();
        assert!(resolved.enabled, "schema default is enabled=true");
        assert_eq!(resolved.path, config::default_history_path());
    }

    /// A `VOICEPI_HISTORY_JSONL=~/history.jsonl` env override must
    /// expand `~` to `$HOME/…` before the sink writes; without this,
    /// `PathBuf::from("~/history.jsonl")` would land in a literal `~`
    /// directory under the current working directory and diverge from
    /// the Python writer / any downstream reader. Codex P2 #620
    /// history_sink.rs:107.
    #[test]
    fn effective_history_settings_tilde_in_env_var_is_expanded() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&[
            "VOICEPI_CONFIG",
            HISTORY_ENABLED_ENV,
            HISTORY_JSONL_ENV,
            "HOME",
            "USERPROFILE",
        ]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::remove_var(HISTORY_ENABLED_ENV);
        std::env::set_var(HISTORY_JSONL_ENV, "~/history.jsonl");
        std::env::set_var("HOME", dir.path());
        std::env::set_var("USERPROFILE", dir.path());

        let resolved = effective_history_settings();
        assert_eq!(
            resolved.path,
            dir.path().join("history.jsonl"),
            "leading `~` must be expanded to $HOME, not written to a literal `~` directory"
        );
    }

    /// The same tilde expansion must apply when the path comes from
    /// `config.json` (config layer wins over env, but both must
    /// expand the same way).
    #[test]
    fn effective_history_settings_tilde_in_config_value_is_expanded() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&[
            "VOICEPI_CONFIG",
            HISTORY_ENABLED_ENV,
            HISTORY_JSONL_ENV,
            "HOME",
            "USERPROFILE",
        ]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(
            &cfg,
            serde_json::json!({ "history_jsonl": "~/history.jsonl" }).to_string(),
        )
        .unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::remove_var(HISTORY_ENABLED_ENV);
        std::env::remove_var(HISTORY_JSONL_ENV);
        std::env::set_var("HOME", dir.path());
        std::env::set_var("USERPROFILE", dir.path());

        let resolved = effective_history_settings();
        assert_eq!(resolved.path, dir.path().join("history.jsonl"));
    }

    /// The reloading sink appends via the shipping `~`-expanded path so
    /// a user with `history_jsonl=~/history.jsonl` in config can `cat`
    /// the file at `$HOME/history.jsonl` after the append -- not at
    /// `./~/history.jsonl` under cwd. Round-trip regression for the
    /// tilde-expansion fix.
    #[test]
    fn reloading_history_sink_writes_to_tilde_expanded_path() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&[
            "VOICEPI_CONFIG",
            HISTORY_ENABLED_ENV,
            HISTORY_JSONL_ENV,
            "HOME",
            "USERPROFILE",
        ]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::set_var(HISTORY_ENABLED_ENV, "1");
        std::env::set_var(HISTORY_JSONL_ENV, "~/history.jsonl");
        std::env::set_var("HOME", dir.path());
        std::env::set_var("USERPROFILE", dir.path());

        let sink = ReloadingHistorySink::new();
        sink.append(&json!({"text": "tilde", "event": "utterance"}));

        let expected = dir.path().join("history.jsonl");
        assert!(
            expected.exists(),
            "sink must write to $HOME/history.jsonl (`~` expanded), not to \
             a literal `~` directory under cwd"
        );
        let literal_tilde = std::path::PathBuf::from("~").join("history.jsonl");
        assert!(
            !literal_tilde.exists(),
            "unexpanded literal `~` path must NOT exist under cwd"
        );
    }

    /// A malformed `config.json` used to drop through to the env layer
    /// (which defaults to `enabled=1`), so a user whose config says
    /// `history_enabled=false` could have dictation persisted while the
    /// file was being rewritten or was transiently invalid. Codex P2
    /// #620 finding `Fail closed when live history config cannot be
    /// read`.
    #[test]
    fn effective_history_settings_reports_config_read_failure() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", HISTORY_ENABLED_ENV, HISTORY_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        // Not JSON.
        std::fs::write(&cfg, "not-json {").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::remove_var(HISTORY_ENABLED_ENV);
        std::env::remove_var(HISTORY_JSONL_ENV);

        let resolved = effective_history_settings();
        assert!(
            resolved.config_error.is_some(),
            "malformed config.json must be surfaced as `config_error` \
             so the sink can fail-closed rather than silently defaulting"
        );
    }

    /// The reloading sink must NOT write on a config-load error, even
    /// when the env layer alone would have defaulted to enabled.
    /// Fail-closed regression.
    #[test]
    fn reloading_history_sink_skips_append_on_config_error() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", HISTORY_ENABLED_ENV, HISTORY_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "definitely not json").unwrap();
        let target = dir.path().join("history.jsonl");
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::set_var(HISTORY_ENABLED_ENV, "1");
        std::env::set_var(HISTORY_JSONL_ENV, &target);

        let sink = ReloadingHistorySink::new();
        sink.append(&json!({"text": "should not land", "event": "utterance"}));
        assert!(
            !target.exists(),
            "config-load error must fail-closed; no history row should be written"
        );
    }

    /// The result-returning variant used by the self-test verb must
    /// bubble an I/O error to the caller (Codex P2 #621
    /// history_write.rs:174: the trait impl swallows the error and
    /// pre-fix the verb hard-coded `Ok(())`).
    #[test]
    fn reloading_history_sink_append_with_result_propagates_io_error() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", HISTORY_ENABLED_ENV, HISTORY_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        std::fs::write(&file_as_parent, "").unwrap();
        let unwritable = file_as_parent.join("history.jsonl");
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::set_var(HISTORY_ENABLED_ENV, "1");
        std::env::set_var(HISTORY_JSONL_ENV, &unwritable);

        let sink = ReloadingHistorySink::new();
        let result = sink.append_with_result(&json!({"text": "boom", "event": "utterance"}));
        assert!(
            result.is_err(),
            "the result-returning variant must surface unwritable-path errors, \
             not swallow them like the shipping `HistorySink::append` does"
        );
    }

    /// Reloading sink: a Settings save between utterances (config file
    /// rewritten to disable history) MUST take effect on the very next
    /// `append` -- without rebuilding the session. Codex P1 #605 finding 2.
    #[test]
    fn reloading_sink_picks_up_config_change_between_appends() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", HISTORY_ENABLED_ENV, HISTORY_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        let hist = dir.path().join("history.jsonl");

        // Round 1: enabled + explicit path. Env unset so config wins.
        std::fs::write(
            &cfg,
            serde_json::json!({
                "history_enabled": "1",
                "history_jsonl": hist.to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::remove_var(HISTORY_ENABLED_ENV);
        std::env::remove_var(HISTORY_JSONL_ENV);

        let sink = ReloadingHistorySink::new();
        sink.append(&json!({"text": "one", "event": "utterance"}));
        let raw1 = std::fs::read_to_string(&hist).unwrap();
        assert_eq!(raw1.lines().count(), 1, "first append must land on disk");

        // Round 2: rewrite config to disable history. Next append is a no-op.
        std::fs::write(
            &cfg,
            serde_json::json!({
                "history_enabled": "0",
                "history_jsonl": hist.to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();

        sink.append(&json!({"text": "two", "event": "utterance"}));
        let raw2 = std::fs::read_to_string(&hist).unwrap();
        assert_eq!(
            raw2.lines().count(),
            1,
            "second append must skip -- config flipped to disabled between utterances"
        );

        // Round 3: rewrite config back to enabled + a NEW path. Next
        // append lands in the new file (path live-reloaded too).
        let hist2 = dir.path().join("history-v2.jsonl");
        std::fs::write(
            &cfg,
            serde_json::json!({
                "history_enabled": "1",
                "history_jsonl": hist2.to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();

        sink.append(&json!({"text": "three", "event": "utterance"}));
        assert!(hist2.exists(), "new path must be picked up on next append");
        let raw3 = std::fs::read_to_string(&hist2).unwrap();
        assert_eq!(raw3.lines().count(), 1);
    }
}
