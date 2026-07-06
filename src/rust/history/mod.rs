//! Local SQLite-backed transcription history (issue #324).
//!
//! Persists one row per accepted utterance in a per-user
//! `history.sqlite3` file so users can review, search, and re-copy past
//! dictations long after the in-memory log has scrolled away. The
//! existing `history.jsonl` sink (`crate::telemetry`) stays in place
//! for now — both run side by side; the SQLite store adds searchable
//! retention with FTS5 + indexed timestamp range scans on top of the
//! flat JSONL.
//!
//! # Module layout
//!
//! * [`schema`] — versioned migrations, FTS5 detection, table /
//!   trigger creation.
//! * [`insert`] — convert the worker-emitted utterance JSON payload
//!   into a typed [`UtteranceRow`] and insert it.
//! * [`search`] — text search (FTS5 when available, LIKE fallback),
//!   date-range filter, pagination.
//!
//! Every file in this module stays well under the 500-LOC AGENTS.md
//! ceiling so each piece keeps its own unit tests.
//!
//! # Wiring
//!
//! The supervisor (`crate::runtime::stream_lines`) calls
//! [`try_record_utterance_default`] on every `event="utterance"`
//! worker event it sees. The call is best-effort: any error is
//! logged to stderr and swallowed so a DB issue (locked file,
//! corrupt schema, missing parent dir) never breaks dictation.
//!
//! # Privacy
//!
//! Users can disable the store by setting `VOICEPI_HISTORY_DISABLED=1`
//! in their environment. The DB path can be overridden via
//! `VOICEPI_HISTORY_DB` (mainly for tests + power users that want the
//! file in a non-default location).

pub mod insert;
pub mod schema;
pub mod search;

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;

pub use insert::{insert_row, insert_utterance, UtteranceRow};
pub use schema::fts5_available;
pub use search::{search, SearchHit, SearchOptions};

/// Env var that disables the SQLite history store at runtime.
///
/// Accepts the truthy values `1`, `true`, `yes`, `on`
/// (case-insensitive, whitespace-trimmed). Anything else — including
/// the default unset state — leaves the store enabled.
pub const HISTORY_DISABLED_ENV: &str = "VOICEPI_HISTORY_DISABLED";
/// Env var that mirrors the documented `history_enabled` schema
/// setting. Codex-P2 finding on #439: the Settings UI toggle and the
/// Python worker both use `VOICEPI_HISTORY_ENABLED=0` (falsy) as
/// "disable history", so the Rust side must honour it too — otherwise a
/// user who opted out via the UI would still see the SQLite store read
/// on the second-hotkey dispatch path.
///
/// Accepts the falsy values `0`, `false`, `no`, `off` (case-insensitive,
/// whitespace-trimmed) — matching the Python `_truthy` helper. Any other
/// value (or the unset state) is treated as "enabled".
pub const HISTORY_ENABLED_ENV: &str = "VOICEPI_HISTORY_ENABLED";
/// Env var that overrides the default DB path. Useful for tests that
/// want a tempdir-scoped DB without mocking `platform_config_dir`, and
/// for power users that want the history on a different volume.
pub const HISTORY_DB_PATH_ENV: &str = "VOICEPI_HISTORY_DB";

/// Resolve the SQLite DB path: the env override wins, otherwise the
/// per-user config directory (same parent as `config.json`).
pub fn default_db_path() -> PathBuf {
    if let Some(raw) = std::env::var_os(HISTORY_DB_PATH_ENV) {
        return PathBuf::from(raw);
    }
    crate::config::platform_config_dir().join("history.sqlite3")
}

/// True unless the user opted out via either the "disable" env var
/// ([`HISTORY_DISABLED_ENV`]) or a falsy "enable" env var
/// ([`HISTORY_ENABLED_ENV`], which mirrors the Settings-UI toggle).
///
/// Codex-P2 finding on #439: honouring both env vars keeps the Rust
/// runtime aligned with the documented `history_enabled` schema field
/// and its Python-side counterpart — a user who disabled history in the
/// UI or exported `VOICEPI_HISTORY_ENABLED=0` will not have the SQLite
/// store secretly read behind their back.
pub fn is_enabled() -> bool {
    if is_truthy_env(HISTORY_DISABLED_ENV) {
        return false;
    }
    if is_falsy_env(HISTORY_ENABLED_ENV) {
        return false;
    }
    true
}

fn is_truthy_env(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .ok()
            .as_deref()
            .map(|raw| raw.trim().to_ascii_lowercase()),
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on")
    )
}

/// Companion to [`is_truthy_env`]: returns `true` when the env var is
/// set to an explicit falsy value (`0`, `false`, `no`, `off`,
/// case-insensitive). An unset variable returns `false` — the caller
/// interprets that as "default", not "disabled".
fn is_falsy_env(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .ok()
            .as_deref()
            .map(|raw| raw.trim().to_ascii_lowercase()),
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off")
    )
}

/// Open (creating if necessary) the SQLite DB at `path`, ensuring the
/// parent directory exists and running pending migrations before
/// returning the connection.
pub fn open(path: impl AsRef<Path>) -> Result<Connection> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(path)?;
    schema::migrate(&conn)?;
    Ok(conn)
}

/// Open the default per-user history DB and migrate it.
pub fn open_default() -> Result<Connection> {
    open(default_db_path())
}

/// Open an in-memory database for tests. Schema is already migrated.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    schema::migrate(&conn)?;
    Ok(conn)
}

/// Insert an utterance row into the default per-user DB. Best-effort
/// wrapper: returns `Ok(0)` (a no-op insert) when the store is
/// disabled by env, and a real `rowid` otherwise.
pub fn record_utterance_default(payload: &Value) -> Result<i64> {
    if !is_enabled() {
        return Ok(0);
    }
    let conn = open_default()?;
    insert_utterance(&conn, payload)
}

/// Fire-and-forget variant used by the runtime supervisor: logs any
/// failure to stderr and swallows it so the supervisor stays alive
/// even if the DB is locked or the disk is full.
///
/// We accept a borrowed `Value` so the supervisor can keep ownership
/// of the worker event for the rest of its dispatch.
pub fn try_record_utterance_default(payload: &Value) {
    if !is_enabled() {
        return;
    }
    if let Err(err) = record_utterance_default(payload) {
        // ASCII-only, AGENTS.md "console output is ASCII- or UTF-8-safe":
        // this hits the supervisor's stderr stream which the UI surfaces
        // verbatim in the runtime log.
        eprintln!("[history] failed to record utterance: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Use a single env-mutation lock since these tests prod
    /// `VOICEPI_HISTORY_DISABLED` / `VOICEPI_HISTORY_DB`, both of
    /// which are process-global.
    use crate::test_env_lock::ENV_LOCK;

    #[test]
    fn is_enabled_defaults_to_true() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var(HISTORY_DISABLED_ENV);
        assert!(is_enabled());
    }

    #[test]
    fn is_enabled_honours_truthy_disabled_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        for raw in ["1", "true", "Yes", " ON ", "TRUE"] {
            std::env::set_var(HISTORY_DISABLED_ENV, raw);
            assert!(
                !is_enabled(),
                "expected disabled for {raw:?}, but is_enabled() was true"
            );
        }
        std::env::remove_var(HISTORY_DISABLED_ENV);
    }

    #[test]
    fn is_enabled_ignores_falsy_or_unknown_disabled_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        for raw in ["", "0", "false", "no", "off", "maybe"] {
            std::env::set_var(HISTORY_DISABLED_ENV, raw);
            assert!(
                is_enabled(),
                "expected enabled for {raw:?}, but is_enabled() was false"
            );
        }
        std::env::remove_var(HISTORY_DISABLED_ENV);
    }

    /// Codex-P2 finding on #439: the documented Settings-UI toggle is
    /// `history_enabled` / `VOICEPI_HISTORY_ENABLED=0`, so honouring it
    /// here keeps the Rust runtime aligned with the Python worker and
    /// the schema field. Every falsy spelling the `_truthy` helper on
    /// the Python side rejects must land as `is_enabled() == false`.
    #[test]
    fn is_enabled_honours_falsy_history_enabled_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var(HISTORY_DISABLED_ENV);
        for raw in ["0", "false", "No", " OFF ", "FALSE"] {
            std::env::set_var(HISTORY_ENABLED_ENV, raw);
            assert!(
                !is_enabled(),
                "expected disabled for VOICEPI_HISTORY_ENABLED={raw:?}, but is_enabled() was true"
            );
        }
        std::env::remove_var(HISTORY_ENABLED_ENV);
    }

    /// Companion: an explicit truthy VOICEPI_HISTORY_ENABLED (or any
    /// unknown value) leaves history enabled — the env var is a gate,
    /// not a strict allow-list.
    #[test]
    fn is_enabled_ignores_truthy_or_unknown_history_enabled_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var(HISTORY_DISABLED_ENV);
        for raw in ["1", "true", "yes", "on", "maybe", ""] {
            std::env::set_var(HISTORY_ENABLED_ENV, raw);
            assert!(
                is_enabled(),
                "expected enabled for VOICEPI_HISTORY_ENABLED={raw:?}, but is_enabled() was false"
            );
        }
        std::env::remove_var(HISTORY_ENABLED_ENV);
    }

    #[test]
    fn default_db_path_honours_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("custom.sqlite3");
        std::env::set_var(HISTORY_DB_PATH_ENV, &path);
        assert_eq!(default_db_path(), path);
        std::env::remove_var(HISTORY_DB_PATH_ENV);
    }

    #[test]
    fn open_creates_parent_directory_and_migrates() {
        let tmp = tempfile::tempdir().unwrap();
        // Nested parent that does NOT yet exist — open() must create it.
        let path = tmp.path().join("nested").join("history.sqlite3");
        let conn = open(&path).unwrap();
        assert!(path.exists());
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert!(version >= 1);
    }

    #[test]
    fn record_utterance_default_persists_to_env_db_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var(HISTORY_DISABLED_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e2e.sqlite3");
        std::env::set_var(HISTORY_DB_PATH_ENV, &path);
        let payload = serde_json::json!({
            "event": "utterance",
            "text": "end-to-end smoke",
            "stt_backend": "whisper",
            "model": "small.en",
            "ts": 1_700_000_000.0_f64,
        });
        // Same path the runtime supervisor exercises on each
        // `event="utterance"` worker event.
        try_record_utterance_default(&payload);
        // Re-open and verify the row landed.
        let conn = open_default().unwrap();
        let (text, backend): (String, String) = conn
            .query_row(
                "SELECT text, stt_backend FROM utterances ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(text, "end-to-end smoke");
        assert_eq!(backend, "whisper");
        std::env::remove_var(HISTORY_DB_PATH_ENV);
    }

    #[test]
    fn record_utterance_default_is_no_op_when_disabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(HISTORY_DISABLED_ENV, "1");
        // Even though the DB env points nowhere sensible, the disabled
        // gate should short-circuit before we touch the filesystem.
        std::env::set_var(HISTORY_DB_PATH_ENV, "/dev/null/does/not/exist.sqlite3");
        let row = record_utterance_default(&serde_json::json!({"text": "hi"})).unwrap();
        assert_eq!(row, 0);
        std::env::remove_var(HISTORY_DISABLED_ENV);
        std::env::remove_var(HISTORY_DB_PATH_ENV);
    }
}
