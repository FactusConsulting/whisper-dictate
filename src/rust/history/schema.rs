//! Versioned schema migrations for the SQLite history store.
//!
//! Migrations are idempotent and additive: each migration is wrapped
//! in `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` so
//! re-running [`migrate`] on an already-current DB is a no-op. The
//! version pointer lives in a one-row `schema_version` table; new
//! columns / indexes go into a NEW migration step so we never mutate
//! a published schema in place.
//!
//! # FTS5
//!
//! The rusqlite `bundled` SQLite build ships with FTS5 enabled, so the
//! virtual-table mirror normally always loads. We still probe support
//! at migrate-time via `PRAGMA compile_options` and skip the FTS5 setup
//! if it is missing, so a system-SQLite build (e.g. distro-packaged
//! rusqlite-without-bundled, future opt-out) degrades to LIKE searches
//! rather than failing the migration outright. Schema bump (`v2`) is
//! still recorded — the only difference is which mirror exists.

use anyhow::Result;
use rusqlite::{params, Connection};

/// Latest supported schema version. Bump every time a new migration
/// step is added below.
pub const CURRENT_VERSION: i64 = 2;

/// Run pending migrations on `conn`, leaving it at [`CURRENT_VERSION`].
///
/// Wrapped in a single transaction per step so a mid-migration failure
/// (e.g. disk full while creating the FTS5 mirror) leaves the DB at
/// the previous version rather than partially upgraded.
pub fn migrate(conn: &Connection) -> Result<()> {
    ensure_version_table(conn)?;
    let mut current = current_version(conn)?;
    // Guard BEFORE the migration loop so a future-version DB (written
    // by a newer build) fails fast on `open()` instead of silently
    // running against a schema we don't know. The match-arm catch-all
    // can never trigger today (we cap stepping at CURRENT_VERSION-1),
    // but kept defensive in case a future migration pattern stops
    // looping linearly.
    if current > CURRENT_VERSION {
        anyhow::bail!(
            "history schema is at version {current} but only \
             migrations up to {CURRENT_VERSION} are known; refusing to \
             overwrite a newer schema (use a newer build)"
        );
    }
    while current < CURRENT_VERSION {
        match current {
            0 => {
                apply_v1(conn)?;
                set_version(conn, 1)?;
            }
            1 => {
                apply_v2(conn)?;
                set_version(conn, 2)?;
            }
            other => {
                anyhow::bail!(
                    "no migration step registered for version {other} → \
                     {}; this is a bug in history::schema::migrate",
                    other + 1
                );
            }
        }
        current += 1;
    }
    Ok(())
}

/// True iff the running SQLite build links FTS5.
///
/// `PRAGMA compile_options` returns one row per build-time option; we
/// look for the `ENABLE_FTS5` token. The rusqlite `bundled` feature
/// turns FTS5 on, so this is `true` for every binary we ship — the
/// detection exists so a custom build (system SQLite without FTS5)
/// degrades gracefully instead of failing the migration.
pub fn fts5_available(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA compile_options")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let option: String = row.get(0)?;
        if option.eq_ignore_ascii_case("ENABLE_FTS5") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_version_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (\
            version INTEGER NOT NULL PRIMARY KEY\
         );",
    )?;
    Ok(())
}

fn current_version(conn: &Connection) -> Result<i64> {
    let mut stmt = conn.prepare("SELECT MAX(version) FROM schema_version")?;
    let value: Option<i64> = stmt.query_row([], |row| row.get(0))?;
    Ok(value.unwrap_or(0))
}

fn set_version(conn: &Connection, version: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO schema_version(version) VALUES (?1)",
        params![version],
    )?;
    Ok(())
}

/// v1: the base `utterances` table and a `ts_unix` index for fast
/// date-range scans (the search API uses `ts_unix BETWEEN ? AND ?`).
fn apply_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "BEGIN;\
         CREATE TABLE IF NOT EXISTS utterances (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            ts TEXT NOT NULL,\
            ts_unix REAL NOT NULL,\
            text TEXT NOT NULL,\
            text_chars INTEGER,\
            stt_backend TEXT,\
            model TEXT,\
            language TEXT,\
            device TEXT,\
            compute_type TEXT,\
            recording_s REAL,\
            audio_duration_s REAL,\
            compute_s REAL,\
            real_time_factor REAL,\
            post_processor TEXT,\
            post_mode TEXT,\
            post_model TEXT,\
            post_changed INTEGER,\
            post_error TEXT,\
            gate TEXT,\
            target_title TEXT,\
            target_process TEXT,\
            raw_payload TEXT NOT NULL\
         );\
         CREATE INDEX IF NOT EXISTS idx_utterances_ts_unix \
            ON utterances(ts_unix);\
         COMMIT;",
    )?;
    Ok(())
}

/// v2: FTS5 mirror of the `text` column + sync triggers. Skipped
/// transparently when the bundled SQLite was compiled without FTS5
/// support (search falls back to LIKE).
fn apply_v2(conn: &Connection) -> Result<()> {
    if !fts5_available(conn)? {
        // Record the version bump anyway: a future SQLite upgrade that
        // adds FTS5 should NOT retroactively try to create the mirror
        // (we'd have to scope it to a new migration step). The
        // search-path code branches on a live `fts5_available` check
        // so the LIKE fallback stays in force until v3+ adds an
        // explicit "create FTS5 mirror now that it's available" step.
        return Ok(());
    }
    conn.execute_batch(
        "BEGIN;\
         CREATE VIRTUAL TABLE IF NOT EXISTS utterances_fts USING fts5(\
            text,\
            content='utterances',\
            content_rowid='id',\
            tokenize='unicode61'\
         );\
         CREATE TRIGGER IF NOT EXISTS utterances_ai \
            AFTER INSERT ON utterances BEGIN \
                INSERT INTO utterances_fts(rowid, text) \
                    VALUES (new.id, new.text); \
            END;\
         CREATE TRIGGER IF NOT EXISTS utterances_ad \
            AFTER DELETE ON utterances BEGIN \
                INSERT INTO utterances_fts(utterances_fts, rowid, text) \
                    VALUES('delete', old.id, old.text); \
            END;\
         CREATE TRIGGER IF NOT EXISTS utterances_au \
            AFTER UPDATE ON utterances BEGIN \
                INSERT INTO utterances_fts(utterances_fts, rowid, text) \
                    VALUES('delete', old.id, old.text); \
                INSERT INTO utterances_fts(rowid, text) \
                    VALUES (new.id, new.text); \
            END;\
         COMMIT;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_db_migrates_to_current_version() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), CURRENT_VERSION);
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        // Running it again must not error and must not bump the
        // version past CURRENT_VERSION.
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), CURRENT_VERSION);
    }

    #[test]
    fn bundled_sqlite_ships_fts5() {
        // The rusqlite `bundled` feature compiles SQLite with
        // SQLITE_ENABLE_FTS5; if a future upgrade silently drops it
        // this guard fails so we notice before search regresses to the
        // LIKE-only path.
        let conn = Connection::open_in_memory().unwrap();
        assert!(fts5_available(&conn).unwrap());
    }

    #[test]
    fn migrate_creates_utterances_and_fts_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','view') ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(tables.contains(&"utterances".to_owned()));
        assert!(tables.contains(&"utterances_fts".to_owned()));
        assert!(tables.contains(&"schema_version".to_owned()));
    }

    #[test]
    fn migrate_creates_ts_unix_index() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(indexes.contains(&"idx_utterances_ts_unix".to_owned()));
    }

    #[test]
    fn migrate_refuses_to_downgrade() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        // Simulate a newer schema written by a future build.
        set_version(&conn, CURRENT_VERSION + 1).unwrap();
        let err = migrate(&conn).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("refusing to overwrite a newer schema"),
            "unexpected error message: {msg}"
        );
    }
}
