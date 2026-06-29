//! Read API for the SQLite history store: substring text search with
//! FTS5 (preferred) or LIKE (fallback), optional date-range filter,
//! and limit/offset pagination.
//!
//! The search path branches on a runtime [`super::fts5_available`]
//! probe rather than a compile-time feature so a degraded SQLite (no
//! FTS5 — e.g. a future opt-out build) still returns reasonable
//! results. FTS5 ranks by `bm25(...)` ascending (lower = better) and
//! we expose the score so callers can show "best match" ordering when
//! desired.

use anyhow::Result;
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};

/// Options for [`search`]. All fields are optional; the default
/// equivalent (`SearchOptions::default()`) returns the most recent
/// rows in `limit` size with no text or date filters applied.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Free-text query. Empty / whitespace-only is treated as "no
    /// text filter".
    pub query: Option<String>,
    /// Inclusive lower bound for `ts_unix` (seconds since epoch).
    pub from_unix: Option<f64>,
    /// Inclusive upper bound for `ts_unix` (seconds since epoch).
    pub to_unix: Option<f64>,
    /// Maximum rows to return. `None` means "no explicit cap" — the
    /// caller probably wants to set this in production. Tests can
    /// leave it `None` to assert all rows are visible.
    pub limit: Option<u32>,
    /// Skip this many leading rows after filtering (pagination).
    pub offset: u32,
}

/// One row of search output. `score` is `Some(_)` only when the query
/// went through the FTS5 path; the LIKE fallback always reports
/// `None` (it has no relevance ranking).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: i64,
    pub ts: String,
    pub ts_unix: f64,
    pub text: String,
    pub stt_backend: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
    pub score: Option<f64>,
}

/// Run a search against `conn`. Returns hits sorted by (FTS5 score
/// ascending) or (`ts_unix` descending) when no text query was given.
pub fn search(conn: &Connection, options: &SearchOptions) -> Result<Vec<SearchHit>> {
    let text_query = options
        .query
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty());
    if let Some(query) = text_query {
        if super::fts5_available(conn)? {
            return run_fts_search(conn, query, options);
        }
        return run_like_search(conn, query, options);
    }
    run_recent_scan(conn, options)
}

fn run_fts_search(
    conn: &Connection,
    query: &str,
    options: &SearchOptions,
) -> Result<Vec<SearchHit>> {
    let mut sql = String::from(
        "SELECT u.id, u.ts, u.ts_unix, u.text, u.stt_backend, u.model, u.language, \
                bm25(utterances_fts) AS score \
         FROM utterances_fts \
         JOIN utterances u ON u.id = utterances_fts.rowid \
         WHERE utterances_fts MATCH ?1",
    );
    let mut params: Vec<SqlValue> = vec![SqlValue::Text(escape_fts_query(query))];
    append_date_filter(&mut sql, &mut params, options, "u.ts_unix");
    sql.push_str(" ORDER BY score ASC, u.ts_unix DESC");
    append_pagination(&mut sql, &mut params, options);
    collect_hits(conn, &sql, params, true)
}

fn run_like_search(
    conn: &Connection,
    query: &str,
    options: &SearchOptions,
) -> Result<Vec<SearchHit>> {
    let mut sql = String::from(
        "SELECT id, ts, ts_unix, text, stt_backend, model, language, NULL \
         FROM utterances \
         WHERE text LIKE ?1 ESCAPE '\\'",
    );
    let mut params: Vec<SqlValue> = vec![SqlValue::Text(format!("%{}%", escape_like(query)))];
    append_date_filter(&mut sql, &mut params, options, "ts_unix");
    sql.push_str(" ORDER BY ts_unix DESC");
    append_pagination(&mut sql, &mut params, options);
    collect_hits(conn, &sql, params, false)
}

fn run_recent_scan(conn: &Connection, options: &SearchOptions) -> Result<Vec<SearchHit>> {
    let mut sql = String::from(
        "SELECT id, ts, ts_unix, text, stt_backend, model, language, NULL \
         FROM utterances \
         WHERE 1=1",
    );
    let mut params: Vec<SqlValue> = Vec::new();
    append_date_filter(&mut sql, &mut params, options, "ts_unix");
    sql.push_str(" ORDER BY ts_unix DESC");
    append_pagination(&mut sql, &mut params, options);
    collect_hits(conn, &sql, params, false)
}

fn append_date_filter(
    sql: &mut String,
    params: &mut Vec<SqlValue>,
    options: &SearchOptions,
    ts_unix_column: &str,
) {
    if let Some(from) = options.from_unix {
        sql.push_str(&format!(" AND {ts_unix_column} >= ?{}", params.len() + 1));
        params.push(SqlValue::Real(from));
    }
    if let Some(to) = options.to_unix {
        sql.push_str(&format!(" AND {ts_unix_column} <= ?{}", params.len() + 1));
        params.push(SqlValue::Real(to));
    }
}

fn append_pagination(sql: &mut String, params: &mut Vec<SqlValue>, options: &SearchOptions) {
    if let Some(limit) = options.limit {
        sql.push_str(&format!(" LIMIT ?{}", params.len() + 1));
        params.push(SqlValue::Integer(i64::from(limit)));
        if options.offset > 0 {
            sql.push_str(&format!(" OFFSET ?{}", params.len() + 1));
            params.push(SqlValue::Integer(i64::from(options.offset)));
        }
    } else if options.offset > 0 {
        // SQLite syntactically requires LIMIT before OFFSET; -1 means
        // "no limit" in SQLite (since the limit is signed).
        sql.push_str(" LIMIT -1");
        sql.push_str(&format!(" OFFSET ?{}", params.len() + 1));
        params.push(SqlValue::Integer(i64::from(options.offset)));
    }
}

fn collect_hits(
    conn: &Connection,
    sql: &str,
    params: Vec<SqlValue>,
    has_score: bool,
) -> Result<Vec<SearchHit>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| {
        Ok(SearchHit {
            id: row.get(0)?,
            ts: row.get(1)?,
            ts_unix: row.get(2)?,
            text: row.get(3)?,
            stt_backend: row.get::<_, Option<String>>(4)?,
            model: row.get::<_, Option<String>>(5)?,
            language: row.get::<_, Option<String>>(6)?,
            score: if has_score {
                row.get::<_, Option<f64>>(7)?
            } else {
                None
            },
        })
    })?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row?);
    }
    Ok(hits)
}

/// Escape an FTS5 MATCH query so user input is treated as a phrase
/// rather than as FTS5 syntax. We wrap the term in double quotes and
/// escape any embedded `"` by doubling it, which is the FTS5 convention.
fn escape_fts_query(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Escape SQL `LIKE` special characters (`%`, `_`, and the chosen
/// escape char `\`) so user input is matched literally.
fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{insert_row, insert_utterance, open_in_memory, UtteranceRow};
    use serde_json::json;

    fn populate() -> Connection {
        let conn = open_in_memory().unwrap();
        // Spread three rows across a 30-day window with stable
        // timestamps so date-range filtering is deterministic.
        let base = 1_700_000_000.0_f64;
        insert_row(
            &conn,
            &UtteranceRow {
                ts: "2023-11-14T00:00:00.000Z".to_owned(),
                ts_unix: base,
                text: "the quick brown fox".to_owned(),
                stt_backend: Some("whisper".to_owned()),
                model: Some("small.en".to_owned()),
                language: Some("en".to_owned()),
                raw_payload: "{}".to_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        insert_row(
            &conn,
            &UtteranceRow {
                ts: "2023-11-15T00:00:00.000Z".to_owned(),
                ts_unix: base + 86_400.0,
                text: "jumps over the lazy dog".to_owned(),
                stt_backend: Some("whisper".to_owned()),
                model: Some("small.en".to_owned()),
                language: Some("en".to_owned()),
                raw_payload: "{}".to_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        insert_row(
            &conn,
            &UtteranceRow {
                ts: "2023-12-14T00:00:00.000Z".to_owned(),
                ts_unix: base + 30.0 * 86_400.0,
                text: "a totally unrelated phrase about cats".to_owned(),
                stt_backend: Some("whisper".to_owned()),
                model: Some("medium".to_owned()),
                language: Some("en".to_owned()),
                raw_payload: "{}".to_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        conn
    }

    #[test]
    fn no_query_returns_rows_sorted_recent_first() {
        let conn = populate();
        let hits = search(&conn, &SearchOptions::default()).unwrap();
        assert_eq!(hits.len(), 3);
        assert!(hits[0].ts_unix > hits[1].ts_unix);
        assert!(hits[1].ts_unix > hits[2].ts_unix);
    }

    #[test]
    fn fts_search_finds_text() {
        let conn = populate();
        let hits = search(
            &conn,
            &SearchOptions {
                query: Some("brown".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].text.contains("brown"));
        assert!(hits[0].score.is_some(), "FTS5 path must report a score");
    }

    #[test]
    fn fts_search_is_whitespace_split_by_default() {
        let conn = populate();
        // Two-word query should find the row that contains both words.
        let hits = search(
            &conn,
            &SearchOptions {
                query: Some("brown fox".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].text.contains("brown"));
    }

    #[test]
    fn search_filters_by_date_range() {
        let conn = populate();
        let base = 1_700_000_000.0_f64;
        let hits = search(
            &conn,
            &SearchOptions {
                from_unix: Some(base - 1.0),
                to_unix: Some(base + 2.0 * 86_400.0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 2);
        // None of the November rows survive a December lower bound.
        let later = search(
            &conn,
            &SearchOptions {
                from_unix: Some(base + 20.0 * 86_400.0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(later.len(), 1);
        assert!(later[0].text.contains("cats"));
    }

    #[test]
    fn pagination_returns_disjoint_pages() {
        let conn = populate();
        let page_a = search(
            &conn,
            &SearchOptions {
                limit: Some(2),
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        let page_b = search(
            &conn,
            &SearchOptions {
                limit: Some(2),
                offset: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page_a.len(), 2);
        assert_eq!(page_b.len(), 1);
        for hit in &page_b {
            assert!(!page_a.iter().any(|other| other.id == hit.id));
        }
    }

    #[test]
    fn like_fallback_matches_when_fts_disabled() {
        // Open a fresh in-memory DB, run only the v1 migration (no
        // FTS5 mirror) and verify the like_search code path returns
        // the right rows. We can't easily turn FTS5 off without
        // forking the migration, so we just exercise the LIKE helper
        // directly through `run_like_search`.
        let conn = populate();
        let hits = run_like_search(
            &conn,
            "lazy",
            &SearchOptions {
                limit: Some(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].text.contains("lazy"));
        assert!(hits[0].score.is_none(), "LIKE path has no relevance score");
    }

    #[test]
    fn like_fallback_escapes_special_characters() {
        // Insert text with `%` and `_` and confirm the literal search
        // doesn't accidentally match unrelated rows via SQL wildcards.
        let conn = open_in_memory().unwrap();
        insert_utterance(&conn, &json!({"text": "100% certain"})).unwrap();
        insert_utterance(&conn, &json!({"text": "snake_case"})).unwrap();
        insert_utterance(&conn, &json!({"text": "uninvolved"})).unwrap();

        let percent = run_like_search(
            &conn,
            "100%",
            &SearchOptions {
                limit: Some(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(percent.len(), 1);
        assert!(percent[0].text.contains("100%"));

        let underscore = run_like_search(
            &conn,
            "snake_case",
            &SearchOptions {
                limit: Some(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(underscore.len(), 1);
        assert!(underscore[0].text.contains("snake_case"));
    }

    #[test]
    fn escape_fts_query_doubles_quotes() {
        assert_eq!(escape_fts_query("foo"), "\"foo\"");
        assert_eq!(escape_fts_query("a \"b\" c"), "\"a \"\"b\"\" c\"");
    }

    #[test]
    fn escape_like_prefixes_specials_with_backslash() {
        assert_eq!(escape_like("ab%cd_ef\\gh"), r"ab\%cd\_ef\\gh");
    }

    #[test]
    fn fts_query_does_not_misinterpret_user_syntax() {
        // Raw FTS5 operators in user input must not be parsed as
        // syntax — the wrap-in-quotes helper turns them into a phrase
        // term. A query like `foo OR bar` would otherwise match any
        // row containing `foo` OR `bar`; here we expect zero matches
        // because no row contains the literal phrase.
        let conn = populate();
        let hits = search(
            &conn,
            &SearchOptions {
                query: Some("brown OR cats".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn offset_without_limit_still_paginates() {
        let conn = populate();
        let hits = search(
            &conn,
            &SearchOptions {
                offset: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 2);
    }
}
