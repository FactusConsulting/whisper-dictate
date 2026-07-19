//! `whisper-dictate self-test whisper-load` regression check.
//!
//! Item 5 prereq 5 (Whisper cold-load latency UX): load a GGML model
//! through the same [`super::local::Preloader`] the supervisor will use
//! in Phase C, report wall-clock elapsed + on-disk file size + a
//! machine-readable status, and exit non-zero on any failure. Compiled
//! only when `whisper-rs-local` is on — a stock build returns an
//! actionable "rebuild with --features" error from the CLI dispatcher.
//!
//! **Why a self-test at all**: v1.20.7's silent-PTT scenario would have
//! been caught earlier by a regression check that actually loaded a
//! model on every merge. This verb runs in the ubuntu-2604 integration
//! job (once the model has been downloaded via `models download`) so a
//! whisper-rs API break, a whisper.cpp OOM regression, or a preloader
//! wiring bug fails CI instead of shipping.
//!
//! Output shape is intentionally similar to `self-test ptt-wedge` and
//! `self-test injection-idempotency` — the smoke script + CI can share a
//! JSON parser.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use super::local::{load_blocking, LoadStatus};
use super::model_manager::{find, is_downloaded, model_path};

/// Upper bound on how long the self-test waits for the load thread to
/// resolve. Chosen well above the observed cold-load times for the
/// large models on a slow VM (large-v3 is ~40 s on a 4-core cloud
/// runner) so we don't false-alarm on CI machines with slow disks.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Machine-readable report emitted with `--json`. Field names mirror
/// the other self-test reports so downstream JQ filters generalise.
#[derive(Debug)]
pub struct WhisperLoadReport {
    /// Catalog model name (`tiny.en`, `base`, …) or `custom` for a
    /// user-supplied path. Preserved from the CLI arg so an operator
    /// running the smoke script sees which model was probed.
    pub model: String,
    /// Absolute path the load attempted.
    pub path: PathBuf,
    /// On-disk file size in bytes. Reported for the sanity-check that
    /// the load actually consumed a real weights file — a zero-byte
    /// report is a sign the model file was truncated (fixed in
    /// #480-era model_manager but worth a belt-and-braces check).
    pub file_size_bytes: u64,
    /// Wall-clock elapsed from `Preloader::start` to the terminal
    /// status. Includes background-thread spawn overhead so the number
    /// matches what the supervisor perceives on cold start.
    pub elapsed_ms: u128,
    /// `loading` (timeout), `ready`, `error`. Values match
    /// [`LoadStatus::label`] so JSON consumers can match on the same
    /// three strings the Python worker emits.
    pub status_label: &'static str,
    /// True iff `status_label == "ready"`. Kept as a top-level flag so
    /// the smoke script's `grep -q '"ok":true'` idiom keeps working
    /// across verbs.
    pub ok: bool,
    /// Filled with the `LoadFailure` message on `Failed`, or with a
    /// timeout message on `Loading`. `None` on success.
    pub error: Option<String>,
    /// `errored` / `panicked` / `null`, matching
    /// [`super::local::LoadFailure::kind`]. Split from `error` so
    /// consumers can distinguish OOM (`panicked`) from a routine
    /// missing-file error without regex parsing.
    pub error_kind: Option<&'static str>,
}

impl WhisperLoadReport {
    /// Compact JSON envelope. Manual escape keeps the crate free of a
    /// `serde` derive on this type — the fields are all safe ASCII
    /// (labels + numbers) except `path` and `error` which use
    /// `json_escape` to survive backslash-heavy Windows paths and
    /// multi-line panic messages.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"kind":"whisper-load","model":{model},"path":{path},"file_size_bytes":{size},"elapsed_ms":{elapsed},"status":{status},"ok":{ok},"error":{error},"error_kind":{error_kind}}}"#,
            model = json_string(&self.model),
            path = json_string(&self.path.display().to_string()),
            size = self.file_size_bytes,
            elapsed = self.elapsed_ms,
            status = json_string(self.status_label),
            ok = self.ok,
            error = self
                .error
                .as_deref()
                .map(json_string)
                .unwrap_or_else(|| "null".to_owned()),
            error_kind = self
                .error_kind
                .map(json_string)
                .unwrap_or_else(|| "null".to_owned()),
        )
    }

    /// Human-readable rendering used when `--json` is not passed.
    pub fn to_plain(&self) -> String {
        let mut out = format!(
            "whisper-load: model={} status={} elapsed={}ms size={} bytes\n  path: {}\n",
            self.model,
            self.status_label,
            self.elapsed_ms,
            self.file_size_bytes,
            self.path.display(),
        );
        if let Some(err) = &self.error {
            out.push_str(&format!(
                "  error ({}): {}\n",
                self.error_kind.unwrap_or("unknown"),
                err
            ));
        }
        out
    }
}

/// Run the load check for `model_name`. Resolution order:
///
/// 1. If `model_name` matches a catalog entry (`tiny.en`, `base`, …) AND
///    the file is downloaded, use its path. This is the normal path.
/// 2. If `model_name` looks like a path (contains `/` or `\\` or ends in
///    `.bin` / `.gguf`), use it verbatim. Handles the power-user "load
///    a custom fine-tune" case the catalog can't cover.
/// 3. Otherwise error with an actionable message that names the missing
///    catalog entry and points at `models download`.
///
/// Returns Ok(report) on both success AND clean load failure — the
/// caller (main.rs) inspects `report.ok` to decide the process exit
/// code. Only truly unexpected errors (couldn't resolve the model
/// name) bubble as `Err`.
pub fn run_whisper_load_test(model_name: &str) -> Result<WhisperLoadReport> {
    let (label, path) = resolve_model(model_name)?;
    let file_size_bytes = std::fs::metadata(&path)
        .with_context(|| format!("stat model file {}", path.display()))?
        .len();

    let (status, _model) = load_blocking(&path, DEFAULT_TIMEOUT);
    let elapsed_ms = status.elapsed().as_millis();
    let status_label = status.label();

    let (ok, error, error_kind) = match &status {
        LoadStatus::Ready { .. } => (true, None, None),
        LoadStatus::Failed { failure, .. } => {
            (false, Some(failure.message()), Some(failure.kind()))
        }
        LoadStatus::Loading { .. } => (
            false,
            Some(format!(
                "load did not complete within {}s (preloader still Loading)",
                DEFAULT_TIMEOUT.as_secs()
            )),
            Some("timeout"),
        ),
        LoadStatus::Consumed { .. } => {
            // Unreachable — load_blocking only Consumes on Ready. Keep
            // the arm so a future refactor of LoadStatus doesn't break
            // compilation silently.
            (true, None, None)
        }
    };

    Ok(WhisperLoadReport {
        model: label,
        path,
        file_size_bytes,
        elapsed_ms,
        status_label,
        ok,
        error,
        error_kind,
    })
}

/// Resolve `--model` to a `(label, path)` tuple. Split out so the
/// unit tests can drive the resolution rules without a real load.
pub(crate) fn resolve_model(model_name: &str) -> Result<(String, PathBuf)> {
    let trimmed = model_name.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "--model must be a catalog name (e.g. tiny.en) or a path to a GGML file"
        ));
    }

    // Path-like → treat as literal.
    if looks_like_path(trimmed) {
        let p = PathBuf::from(trimmed);
        if !p.is_file() {
            return Err(anyhow!(
                "--model {trimmed} looks like a path but no file exists there"
            ));
        }
        return Ok(("custom".to_owned(), p));
    }

    // Catalog lookup.
    let entry = find(trimmed).ok_or_else(|| {
        anyhow!(
            "--model {trimmed} does not match any catalog entry; try one of: tiny.en, \
             base.en, small.en, tiny, base, small, medium, large-v3-turbo, large-v3 \
             (or pass a path to a custom GGML file)"
        )
    })?;
    if !is_downloaded(entry) {
        return Err(anyhow!(
            "--model {trimmed} is a valid catalog entry but the GGML file is not in the \
             cache; run `whisper-dictate models download {trimmed}` first"
        ));
    }
    let p = model_path(entry).context("resolve model cache path")?;
    Ok((entry.name.to_owned(), p))
}

fn looks_like_path(s: &str) -> bool {
    s.contains('/') || s.contains('\\') || s.ends_with(".bin") || s.ends_with(".gguf")
}

/// Minimal JSON string escaper for the report's stringly-typed fields.
/// Not a general escape (no unicode escape sequences); the two contexts
/// that pass through it are file paths (backslashes on Windows) and
/// `anyhow` error messages (may contain newlines from context chains).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_rejects_empty() {
        let err = resolve_model("   ").expect_err("empty must fail");
        assert!(err.to_string().contains("catalog name"), "{err}");
    }

    #[test]
    fn resolve_model_rejects_bogus_path() {
        let err = resolve_model("/definitely/not/a/real/path.bin").expect_err("must fail");
        assert!(err.to_string().contains("no file exists"), "{err}");
    }

    #[test]
    fn resolve_model_rejects_unknown_catalog_name() {
        let err = resolve_model("mystery-model").expect_err("must fail");
        // Actionable message must list at least one catalog entry.
        assert!(err.to_string().contains("tiny.en"), "{err}");
    }

    #[test]
    fn looks_like_path_recognises_common_shapes() {
        assert!(looks_like_path("/home/x/model.bin"));
        assert!(looks_like_path(r"C:\models\ggml-tiny.en.bin"));
        assert!(looks_like_path("ggml-tiny.en.bin"));
        assert!(looks_like_path("something.gguf"));
        assert!(!looks_like_path("tiny.en"));
        assert!(!looks_like_path("base"));
    }

    #[test]
    fn json_string_escapes_backslashes_and_quotes() {
        let s = json_string(r#"C:\path\with"quote"#);
        assert_eq!(s, r#""C:\\path\\with\"quote""#);
    }

    #[test]
    fn json_string_escapes_control_chars() {
        assert_eq!(json_string("a\nb"), r#""a\nb""#);
        assert_eq!(json_string("a\tb"), r#""a\tb""#);
    }

    #[test]
    fn report_json_envelope_shape() {
        let report = WhisperLoadReport {
            model: "tiny.en".to_owned(),
            path: PathBuf::from("/tmp/ggml-tiny.en.bin"),
            file_size_bytes: 78_000_000,
            elapsed_ms: 812,
            status_label: "ready",
            ok: true,
            error: None,
            error_kind: None,
        };
        let json = report.to_json();
        assert!(json.contains(r#""kind":"whisper-load""#));
        assert!(json.contains(r#""model":"tiny.en""#));
        assert!(json.contains(r#""status":"ready""#));
        assert!(json.contains(r#""ok":true"#));
        assert!(json.contains(r#""error":null"#));
        assert!(json.contains(r#""error_kind":null"#));
        assert!(json.contains(r#""elapsed_ms":812"#));
    }

    #[test]
    fn report_json_encodes_error_shape() {
        let report = WhisperLoadReport {
            model: "tiny.en".to_owned(),
            path: PathBuf::from("/tmp/ggml-tiny.en.bin"),
            file_size_bytes: 0,
            elapsed_ms: 42,
            status_label: "error",
            ok: false,
            error: Some("out of memory".to_owned()),
            error_kind: Some("panicked"),
        };
        let json = report.to_json();
        assert!(json.contains(r#""ok":false"#));
        assert!(json.contains(r#""error":"out of memory""#));
        assert!(json.contains(r#""error_kind":"panicked""#));
    }
}
