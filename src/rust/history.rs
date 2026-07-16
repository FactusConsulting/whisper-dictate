//! Public `whisper-dictate history …` CLI verbs.
//!
//! Wraps the existing JSONL history library (writer + preview in
//! [`crate::telemetry`]) with scripting-friendly reader verbs:
//!
//! * `history list [N]` — legacy verb, human-readable tail (delegates to
//!   [`telemetry::preview_jsonl`]).
//! * `history last [--n N] [--json]` — the N most recent transcripts,
//!   newest first. Default `--n 1` and plain text preserves the old
//!   `history last` behaviour for scripts that just want the last text.
//! * `history copy-last` — pipe the last transcript into the OS clipboard
//!   via a subprocess (`wl-copy` / `xclip` / `clip.exe` / `pbcopy`), so no
//!   new Rust clipboard dep is pulled in.
//! * `history reinject-last [--dry-run|--do-it] [--json]` — pull the last
//!   transcript and feed it back through the public `inject-text` plan.
//!   **Defaults to a dry-run** (safe): nothing is typed unless `--do-it`.
//! * `history search <QUERY> [--limit N] [--json]` — substring search over
//!   the transcript `text` field, newest first, capped at `--limit`.
//!
//! Audit item 2 chunk D — see
//! `docs/architecture-audit-2026-07-16.md` line 493 & 307 for the mission.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::cli::HistoryCommand;
use crate::config;
use crate::injection;
use crate::telemetry;

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Route the parsed `history` subcommand to its handler.
///
/// The path is resolved from settings on every call (mirrors the Python
/// `vp_history.history_path` behaviour) so setting `history_jsonl` in the
/// config file takes effect without a restart.
pub fn handle_history_command(command: HistoryCommand) -> Result<()> {
    let path = telemetry::history_path_from_settings()?;
    match command {
        HistoryCommand::List { limit } => list(&path, limit),
        HistoryCommand::Last { n, json } => last(&path, n, json, &mut StdoutSink),
        HistoryCommand::CopyLast => copy_last(&path, &mut SubprocessClipboard, &mut StdoutSink),
        HistoryCommand::ReinjectLast {
            dry_run,
            do_it,
            json,
            backend,
        } => reinject_last(&path, &backend, dry_run, do_it, json),
        HistoryCommand::Search { query, limit, json } => {
            search(&path, &query, limit, json, &mut StdoutSink)
        }
    }
}

// ---------------------------------------------------------------------------
// Pure JSONL readers — no I/O once the file bytes are read.
// ---------------------------------------------------------------------------

/// Read every valid JSONL row from `path`. Missing file returns `Ok(vec![])`
/// (the fresh-install case — no error). Malformed lines are silently
/// dropped so a single corrupt row cannot break the whole read.
///
/// Return order matches file order (oldest first). Callers that want newest
/// first should reverse the tail themselves.
pub fn read_rows(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    Ok(rows_from_str(&raw))
}

/// Parse a JSONL blob into rows — same semantics as [`read_rows`] minus the
/// file I/O so the unit tests can pin the parser without touching disk.
pub fn rows_from_str(raw: &str) -> Vec<Value> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

/// The most recent row (i.e. the last valid JSONL line).
pub fn last_row(path: &Path) -> Result<Option<Value>> {
    Ok(read_rows(path)?.pop())
}

/// The most recent `n` rows in newest-first order. `n` is clamped to `>=1`
/// so scripts that hand through user input (`--n 0`) get sensible behaviour.
pub fn last_n(path: &Path, n: usize) -> Result<Vec<Value>> {
    let n = n.max(1);
    let mut rows = read_rows(path)?;
    let start = rows.len().saturating_sub(n);
    let tail = rows.split_off(start);
    Ok(tail.into_iter().rev().collect())
}

/// Substring search over the `text` field of every row (case-insensitive),
/// newest first, capped at `limit` (clamped to `>=1`).
pub fn search_rows(path: &Path, query: &str, limit: usize) -> Result<Vec<Value>> {
    let needle = query.to_lowercase();
    let limit = limit.max(1);
    let rows = read_rows(path)?;
    let mut matches: Vec<Value> = rows
        .into_iter()
        .rev()
        .filter(|row| {
            row.get("text")
                .and_then(Value::as_str)
                .map(|text| text.to_lowercase().contains(&needle))
                .unwrap_or(false)
        })
        .take(limit)
        .collect();
    // matches is already newest-first because we iterated `.rev()` first.
    // Keep the vec as-is; return it.
    matches.shrink_to_fit();
    Ok(matches)
}

/// Extract the `text` field from a row as a `String` — history entries
/// without a `text` field are treated as empty (the writer filter keeps
/// `text` for every accepted entry, so this only fires on hand-edited
/// files).
fn row_text(row: &Value) -> String {
    row.get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

// ---------------------------------------------------------------------------
// Handler bodies (thin — most work is in the pure readers above).
// ---------------------------------------------------------------------------

/// `history list [N]` — kept as a thin re-export of the legacy behaviour so
/// downstream tooling (the Python `_run_rust_history_command("list", …)`
/// caller) sees no shape change.
fn list(path: &PathBuf, limit: usize) -> Result<()> {
    let preview = telemetry::preview_jsonl(path, limit)?;
    if !preview.text.is_empty() {
        println!("{}", preview.text);
    }
    Ok(())
}

/// `history last [--n N] [--json]`.
fn last(path: &Path, n: usize, json: bool, sink: &mut dyn LineSink) -> Result<()> {
    let rows = last_n(path, n)?;
    if json {
        sink.line(&serde_json::to_string(&rows)?)?;
        return Ok(());
    }
    for row in &rows {
        let text = row_text(row);
        // Preserve the pre-existing behaviour of `history last` (plain
        // text, no `[]` wrapping) — one line per entry, newest first.
        sink.line(&text)?;
    }
    Ok(())
}

/// `history copy-last`. Fails cleanly on empty history + on missing
/// clipboard tools so the smoke script can distinguish "no transcript yet"
/// from "clipboard broken".
fn copy_last(
    path: &Path,
    clipboard: &mut dyn ClipboardWriter,
    sink: &mut dyn LineSink,
) -> Result<()> {
    let Some(row) = last_row(path)? else {
        return Err(anyhow!("history is empty: no transcript to copy"));
    };
    let text = row_text(&row);
    if text.is_empty() {
        return Err(anyhow!(
            "history is empty: most recent entry has no `text` field"
        ));
    }
    clipboard.copy(&text)?;
    sink.line(&format!("copied: {text}"))?;
    Ok(())
}

/// `history reinject-last`. Reads the last transcript then delegates to
/// [`injection::handle_public_inject_text`] so the dry-run guardrails,
/// backend selection, and JSON output stay in one place.
fn reinject_last(path: &Path, backend: &str, dry_run: bool, do_it: bool, json: bool) -> Result<()> {
    let Some(row) = last_row(path)? else {
        return Err(anyhow!("history is empty: no transcript to reinject"));
    };
    let text = row_text(&row);
    if text.is_empty() {
        return Err(anyhow!(
            "history is empty: most recent entry has no `text` field"
        ));
    }
    injection::handle_public_inject_text(&text, backend, dry_run, do_it, json, "", "")
}

/// `history search`.
fn search(
    path: &Path,
    query: &str,
    limit: usize,
    json: bool,
    sink: &mut dyn LineSink,
) -> Result<()> {
    if query.trim().is_empty() {
        return Err(anyhow!("history search: query must not be empty"));
    }
    let matches = search_rows(path, query, limit)?;
    if json {
        sink.line(&serde_json::to_string(&matches)?)?;
        return Ok(());
    }
    if matches.is_empty() {
        // Non-fatal: print nothing on stdout, exit 0 — mirrors `grep` on
        // no matches. Scripts can pin exit 0 + empty stdout to mean "no
        // hits".
        return Ok(());
    }
    for row in &matches {
        // Include ts + backend in the plain-text form so pipelines can
        // eyeball when the match happened without parsing JSON.
        let ts = row
            .get("ts")
            .map(|v| {
                if let Some(s) = v.as_str() {
                    s.to_owned()
                } else {
                    v.to_string()
                }
            })
            .unwrap_or_else(|| "-".to_owned());
        let backend = row.get("stt_backend").and_then(Value::as_str).unwrap_or("");
        let text = row_text(row);
        sink.line(&format!("{ts} [{backend}] {text}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Small abstractions for testability.
// ---------------------------------------------------------------------------

/// One-line output sink — swappable so unit tests can capture stdout
/// without process-wide `println!` interception.
pub trait LineSink {
    fn line(&mut self, text: &str) -> Result<()>;
}

/// Real stdout sink used by the CLI dispatch.
struct StdoutSink;

impl LineSink for StdoutSink {
    fn line(&mut self, text: &str) -> Result<()> {
        println!("{text}");
        Ok(())
    }
}

/// In-memory sink for tests — collects each emitted line.
#[derive(Default)]
pub struct BufferSink {
    pub lines: Vec<String>,
}

impl LineSink for BufferSink {
    fn line(&mut self, text: &str) -> Result<()> {
        self.lines.push(text.to_owned());
        Ok(())
    }
}

/// System clipboard writer — one attempt per configured backend.
pub trait ClipboardWriter {
    fn copy(&mut self, text: &str) -> Result<()>;
}

/// Subprocess-based clipboard. Tries the OS-appropriate backends in order
/// and returns the FIRST that accepts our stdin (the rest are silent). No
/// new native deps — this reuses whatever the user already has installed
/// (`wl-copy`, `xclip`, `xsel`, `clip.exe`, `pbcopy`).
pub struct SubprocessClipboard;

impl ClipboardWriter for SubprocessClipboard {
    fn copy(&mut self, text: &str) -> Result<()> {
        let candidates = clipboard_candidates(std::env::consts::OS, is_wayland());
        let mut errors: Vec<String> = Vec::new();
        for (program, args) in candidates {
            match run_clipboard_cmd(program, &args, text) {
                Ok(()) => return Ok(()),
                Err(err) => errors.push(format!("{program}: {err}")),
            }
        }
        Err(anyhow!(
            "no clipboard backend succeeded (install wl-clipboard / xclip / xsel on Linux). tried: {}",
            if errors.is_empty() {
                "no candidates for this platform".to_owned()
            } else {
                errors.join("; ")
            }
        ))
    }
}

/// Candidate list per OS + session type. Pure so the tests can pin every
/// platform's ordering without touching the process env.
pub fn clipboard_candidates(os: &str, wayland: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    match os {
        "linux" => {
            if wayland {
                vec![
                    ("wl-copy", vec![]),
                    ("xclip", vec!["-selection", "clipboard"]),
                    ("xsel", vec!["--clipboard", "--input"]),
                ]
            } else {
                vec![
                    ("xclip", vec!["-selection", "clipboard"]),
                    ("xsel", vec!["--clipboard", "--input"]),
                    ("wl-copy", vec![]),
                ]
            }
        }
        "macos" => vec![("pbcopy", vec![])],
        "windows" => vec![("clip.exe", vec![]), ("clip", vec![])],
        // Unknown platform — best-effort try the POSIX tools.
        _ => vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("pbcopy", vec![]),
        ],
    }
}

fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

fn run_clipboard_cmd(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| anyhow!("spawn failed: {err}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("failed to open stdin"))?;
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!("exit status {status}"));
    }
    Ok(())
}

// Silences unused-import warnings on builds where the config re-export path
// only lands via telemetry — keeps the module compile-clean without adding
// a cfg gate.
#[allow(dead_code)]
fn _keep_config_link() -> Option<()> {
    let _ = config::default_history_path;
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmpfile(contents: &str) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        fs::write(&path, contents).unwrap();
        (dir, path)
    }

    // -- pure readers --------------------------------------------------------

    #[test]
    fn read_rows_missing_file_is_empty_no_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.jsonl");
        let rows = read_rows(&path).unwrap();
        assert!(rows.is_empty(), "missing file should read as no rows");
    }

    #[test]
    fn rows_from_str_skips_blank_and_malformed_lines() {
        let raw = "{\"text\":\"one\"}\n\nnot json\n   \n{\"text\":\"two\"}\n";
        let rows = rows_from_str(raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["text"], "one");
        assert_eq!(rows[1]["text"], "two");
    }

    #[test]
    fn last_row_returns_final_valid_entry() {
        let (_dir, path) =
            tmpfile("{\"text\":\"first\"}\n{\"text\":\"middle\"}\n{\"text\":\"final\"}\n");
        let row = last_row(&path).unwrap().unwrap();
        assert_eq!(row["text"], "final");
    }

    #[test]
    fn last_row_on_empty_file_is_none() {
        let (_dir, path) = tmpfile("");
        assert!(last_row(&path).unwrap().is_none());
    }

    #[test]
    fn last_n_returns_newest_first_and_clamps_zero_to_one() {
        let (_dir, path) =
            tmpfile("{\"text\":\"a\"}\n{\"text\":\"b\"}\n{\"text\":\"c\"}\n{\"text\":\"d\"}\n");
        let three = last_n(&path, 3).unwrap();
        assert_eq!(three.len(), 3);
        assert_eq!(three[0]["text"], "d");
        assert_eq!(three[1]["text"], "c");
        assert_eq!(three[2]["text"], "b");

        // n=0 clamps to 1 (safety-net for scripted --n input).
        let clamped = last_n(&path, 0).unwrap();
        assert_eq!(clamped.len(), 1);
        assert_eq!(clamped[0]["text"], "d");
    }

    #[test]
    fn last_n_larger_than_file_returns_everything_newest_first() {
        let (_dir, path) = tmpfile("{\"text\":\"a\"}\n{\"text\":\"b\"}\n");
        let all = last_n(&path, 99).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["text"], "b");
        assert_eq!(all[1]["text"], "a");
    }

    #[test]
    fn search_rows_is_case_insensitive_and_newest_first() {
        let (_dir, path) = tmpfile(
            "{\"text\":\"hello Codex world\"}\n\
             {\"text\":\"nope\"}\n\
             {\"text\":\"CODEX again\"}\n",
        );
        let hits = search_rows(&path, "codex", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["text"], "CODEX again"); // newest first
        assert_eq!(hits[1]["text"], "hello Codex world");
    }

    #[test]
    fn search_rows_respects_limit() {
        let (_dir, path) = tmpfile(
            "{\"text\":\"a match\"}\n\
             {\"text\":\"b match\"}\n\
             {\"text\":\"c match\"}\n",
        );
        let hits = search_rows(&path, "match", 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["text"], "c match");
        assert_eq!(hits[1]["text"], "b match");
    }

    // -- handler bodies (via LineSink + Fake clipboard) ---------------------

    #[test]
    fn last_json_emits_array_of_full_entries_newest_first() {
        let (_dir, path) = tmpfile("{\"text\":\"a\",\"ts\":1}\n{\"text\":\"b\",\"ts\":2}\n");
        let mut sink = BufferSink::default();
        last(&path, 2, true, &mut sink).unwrap();
        assert_eq!(sink.lines.len(), 1);
        let parsed: Value = serde_json::from_str(&sink.lines[0]).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "b"); // newest first
        assert_eq!(arr[1]["text"], "a");
    }

    #[test]
    fn last_plain_prints_one_text_per_line_newest_first() {
        let (_dir, path) = tmpfile("{\"text\":\"a\"}\n{\"text\":\"b\"}\n{\"text\":\"c\"}\n");
        let mut sink = BufferSink::default();
        last(&path, 2, false, &mut sink).unwrap();
        assert_eq!(sink.lines, vec!["c".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn last_on_empty_history_emits_nothing_and_succeeds() {
        let (_dir, path) = tmpfile("");
        let mut sink = BufferSink::default();
        last(&path, 3, false, &mut sink).unwrap();
        assert!(sink.lines.is_empty());
    }

    struct FakeClipboard {
        writes: Vec<String>,
        fail: bool,
    }

    impl FakeClipboard {
        fn new() -> Self {
            Self {
                writes: Vec::new(),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                writes: Vec::new(),
                fail: true,
            }
        }
    }

    impl ClipboardWriter for FakeClipboard {
        fn copy(&mut self, text: &str) -> Result<()> {
            if self.fail {
                return Err(anyhow!("no clipboard"));
            }
            self.writes.push(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn copy_last_writes_text_and_prints_receipt() {
        let (_dir, path) = tmpfile("{\"text\":\"pi is 3.14\"}\n");
        let mut clip = FakeClipboard::new();
        let mut sink = BufferSink::default();
        copy_last(&path, &mut clip, &mut sink).unwrap();
        assert_eq!(clip.writes, vec!["pi is 3.14".to_owned()]);
        assert_eq!(sink.lines, vec!["copied: pi is 3.14".to_owned()]);
    }

    #[test]
    fn copy_last_errors_on_empty_history() {
        let (_dir, path) = tmpfile("");
        let mut clip = FakeClipboard::new();
        let mut sink = BufferSink::default();
        let err = copy_last(&path, &mut clip, &mut sink).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("empty"),
            "error should mention empty history: {err}"
        );
        assert!(clip.writes.is_empty());
        assert!(sink.lines.is_empty());
    }

    #[test]
    fn copy_last_errors_when_row_has_no_text_field() {
        let (_dir, path) = tmpfile("{\"ts\":1}\n");
        let mut clip = FakeClipboard::new();
        let mut sink = BufferSink::default();
        let err = copy_last(&path, &mut clip, &mut sink).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("empty"),
            "err={err}"
        );
    }

    #[test]
    fn copy_last_surfaces_clipboard_backend_failure() {
        let (_dir, path) = tmpfile("{\"text\":\"hello\"}\n");
        let mut clip = FakeClipboard::failing();
        let mut sink = BufferSink::default();
        let err = copy_last(&path, &mut clip, &mut sink).unwrap_err();
        assert!(err.to_string().contains("no clipboard"));
    }

    // -- reinject-last --------------------------------------------------------
    //
    // Full end-to-end injection needs a display server. We assert the
    // *chaining* contract: on empty history reinject-last exits with an
    // "empty" error (same phrasing family as copy-last) so the smoke script's
    // grep pattern (`no history\|empty`) works.

    #[test]
    fn reinject_last_errors_on_empty_history_with_smoke_matching_phrasing() {
        let (_dir, path) = tmpfile("");
        let err = reinject_last(&path, "auto", true, false, false).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("empty") || msg.contains("no history"),
            "reinject error must contain \"empty\" or \"no history\" for the smoke script grep: {err}"
        );
    }

    #[test]
    fn reinject_last_errors_on_row_without_text() {
        let (_dir, path) = tmpfile("{\"ts\":1}\n");
        let err = reinject_last(&path, "auto", true, false, false).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("empty") || msg.contains("no history"),
            "err={err}"
        );
    }

    // -- search ---------------------------------------------------------------

    #[test]
    fn search_json_emits_array_of_matches_newest_first() {
        let (_dir, path) = tmpfile(
            "{\"text\":\"hi codex\",\"ts\":1}\n\
             {\"text\":\"nope\",\"ts\":2}\n\
             {\"text\":\"codex rules\",\"ts\":3}\n",
        );
        let mut sink = BufferSink::default();
        search(&path, "codex", 10, true, &mut sink).unwrap();
        assert_eq!(sink.lines.len(), 1);
        let parsed: Value = serde_json::from_str(&sink.lines[0]).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "codex rules");
        assert_eq!(arr[1]["text"], "hi codex");
    }

    #[test]
    fn search_plain_prints_formatted_matches() {
        let (_dir, path) = tmpfile(
            "{\"text\":\"first codex\",\"ts\":\"2026-07-16T10:00\",\"stt_backend\":\"whisper\"}\n\
             {\"text\":\"second codex\",\"ts\":\"2026-07-16T11:00\",\"stt_backend\":\"cloud\"}\n",
        );
        let mut sink = BufferSink::default();
        search(&path, "codex", 5, false, &mut sink).unwrap();
        assert_eq!(sink.lines.len(), 2);
        // Newest first, plain-text format includes ts, backend, text.
        assert!(sink.lines[0].contains("2026-07-16T11:00"));
        assert!(sink.lines[0].contains("[cloud]"));
        assert!(sink.lines[0].contains("second codex"));
        assert!(sink.lines[1].contains("first codex"));
    }

    #[test]
    fn search_empty_query_is_rejected() {
        let (_dir, path) = tmpfile("{\"text\":\"whatever\"}\n");
        let mut sink = BufferSink::default();
        let err = search(&path, "   ", 5, false, &mut sink).unwrap_err();
        assert!(err.to_string().contains("empty"), "err={err}");
    }

    #[test]
    fn search_no_matches_exits_ok_with_empty_output() {
        let (_dir, path) = tmpfile("{\"text\":\"nothing here\"}\n");
        let mut sink = BufferSink::default();
        search(&path, "xyz", 5, false, &mut sink).unwrap();
        assert!(sink.lines.is_empty(), "no matches should print nothing");
    }

    // -- clipboard candidate matrix ------------------------------------------

    #[test]
    fn clipboard_candidates_wayland_prefers_wl_copy() {
        let cands = clipboard_candidates("linux", true);
        assert_eq!(cands[0].0, "wl-copy");
        // Fallbacks always include xclip so users on GNOME Wayland with
        // XWayland still succeed if wl-clipboard isn't installed.
        assert!(cands.iter().any(|(p, _)| *p == "xclip"));
    }

    #[test]
    fn clipboard_candidates_linux_x11_prefers_xclip() {
        let cands = clipboard_candidates("linux", false);
        assert_eq!(cands[0].0, "xclip");
        assert!(cands.iter().any(|(p, _)| *p == "wl-copy"));
    }

    #[test]
    fn clipboard_candidates_windows_uses_clip_exe() {
        let cands = clipboard_candidates("windows", false);
        assert!(cands.iter().any(|(p, _)| *p == "clip.exe"));
    }

    #[test]
    fn clipboard_candidates_macos_uses_pbcopy() {
        let cands = clipboard_candidates("macos", false);
        assert_eq!(cands[0].0, "pbcopy");
    }
}
