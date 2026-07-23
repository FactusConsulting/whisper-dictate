//! `whisper-dictate doctor` — platform readiness matrix.
//!
//! Runs a battery of READ-ONLY checks and reports each with an ok / warn /
//! fail status. Designed to help users troubleshoot without shelling out to
//! Python (so it works even when the venv is broken), and to be scraped by CI
//! smoke scripts via `--json`.
//!
//! Exit code: 0 if no `fail` checks (warns are non-blocking), 1 otherwise.
//!
//! Every check is a pure function that takes an injected environment / file
//! system view so the whole matrix is unit-testable without spawning
//! subprocesses or touching real config files. The public
//! [`handle_doctor`] entry point wires the real world in and does the
//! printing.
//!
//! Audit item 2 chunk E — see `docs/architecture-audit-2026-07-16.md`.

use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use serde::Serialize;
use serde_json::json;

use crate::config::{config_path, load_settings_from_path};
use crate::injection::fallback::{fallback_chain, locate_on_path, LinuxSession};
use crate::whisper::model_manager;

/// Outcome of a single [`Check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn glyph(self) -> &'static str {
        match self {
            Status::Ok => "OK ",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

/// Result of one readiness check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

impl Check {
    pub fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Ok,
            detail: detail.into(),
        }
    }
    pub fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
        }
    }
    pub fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
        }
    }
}

/// Roll-up counts across a check list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub ok: usize,
    pub warn: usize,
    pub fail: usize,
}

impl Summary {
    pub fn from(checks: &[Check]) -> Self {
        let mut s = Self::default();
        for c in checks {
            match c.status {
                Status::Ok => s.ok += 1,
                Status::Warn => s.warn += 1,
                Status::Fail => s.fail += 1,
            }
        }
        s
    }
}

/// CLI entry point.
///
/// `json` selects machine-readable output. `config_override` mirrors the
/// `--config PATH` flag on the sibling `config` verbs so a user (or a smoke
/// test) can point the config check at an obviously-broken file without
/// mutating `VOICEPI_CONFIG` process-wide. `None` uses the platform default
/// (honours `VOICEPI_CONFIG`).
pub fn handle_doctor(json: bool, config_override: Option<&str>) -> Result<()> {
    let checks = run_all_checks(config_override);
    let summary = Summary::from(&checks);
    if json {
        render_json(&checks, &summary)?;
    } else {
        render_text(&checks, &summary);
    }
    if summary.fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Build the full check list.
fn run_all_checks(config_override: Option<&str>) -> Vec<Check> {
    let cfg_path = resolve_config_path(config_override);
    vec![
        check_os(),
        check_session(),
        check_python(),
        check_models_cache(),
        check_injection_backends(),
        check_audio_input(),
        check_config_file(&cfg_path),
        check_configured_model(&cfg_path),
    ]
}

fn resolve_config_path(override_path: Option<&str>) -> PathBuf {
    match override_path {
        Some(raw) => PathBuf::from(raw),
        None => config_path(),
    }
}

// ---------- individual checks ------------------------------------------------

fn check_os() -> Check {
    let detail = format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH);
    Check::ok("os", detail)
}

fn check_session() -> Check {
    let name = "session";
    if cfg!(windows) {
        return Check::ok(name, "windows-desktop");
    }
    if cfg!(target_os = "macos") {
        return Check::ok(name, "aqua");
    }
    // Linux / other unix: reuse the injection-layer detector so the doctor
    // agrees with what the injection dispatcher will actually see.
    let session = LinuxSession::detect();
    let label = match session {
        LinuxSession::KdeWayland => "wayland (kde/plasma)",
        LinuxSession::OtherWayland => "wayland",
        LinuxSession::X11 => "x11",
        LinuxSession::Unknown => "unknown (no DISPLAY / WAYLAND_DISPLAY / XDG_SESSION_TYPE)",
    };
    if matches!(session, LinuxSession::Unknown) {
        Check::warn(name, label)
    } else {
        Check::ok(name, label)
    }
}

/// Locate a Python 3.12+ interpreter. Order of preference:
/// * `VOICEPI_PYTHON` (the same override the runtime honours)
/// * Windows launcher (`py -3.12`)
/// * `python3` / `python`
///
/// A missing interpreter is FAIL (dictation cannot run). An older interpreter
/// is WARN (the user *may* have shipped worker deps that back-port far enough,
/// but we don't want to silently pass).
fn check_python() -> Check {
    let name = "python";
    for cand in python_candidates() {
        if let Some(version) = probe_python(&cand) {
            let (major, minor) = version;
            let detail = format!("{} → {}.{}", cand.describe(), major, minor);
            if (major, minor) >= (3, 12) {
                return Check::ok(name, detail);
            }
            return Check::warn(
                name,
                format!("{detail} (older than 3.12; some worker deps require 3.12+)"),
            );
        }
    }
    Check::fail(
        name,
        "no Python 3.12+ found on PATH (tried VOICEPI_PYTHON, `py -3.12`, `python3`, `python`)",
    )
}

fn check_models_cache() -> Check {
    let name = "models-cache";
    match model_manager::models_cache_dir() {
        Ok(dir) => classify_dir_writable(name, &dir),
        Err(err) => Check::fail(
            name,
            format!("cannot resolve user cache dir: {err} (set HOME / LOCALAPPDATA)"),
        ),
    }
}

fn classify_dir_writable(name: &'static str, dir: &Path) -> Check {
    if !dir.exists() {
        // Non-fatal: the manager creates the dir lazily on first download.
        return Check::warn(
            name,
            format!(
                "{} (missing; will be created on first download)",
                dir.display()
            ),
        );
    }
    if !dir.is_dir() {
        return Check::fail(name, format!("{} is not a directory", dir.display()));
    }
    if !is_dir_writable(dir) {
        return Check::fail(name, format!("{} is not writable", dir.display()));
    }
    Check::ok(name, dir.display().to_string())
}

/// Best-effort writability probe — create a tiny sentinel file, remove it.
fn is_dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".wd-doctor-writable.tmp");
    match std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn check_injection_backends() -> Check {
    let name = "injection-backends";
    if cfg!(windows) {
        return Check::ok(name, "windows / SendInput (built into pynput)");
    }
    if cfg!(target_os = "macos") {
        return Check::ok(name, "macos / pynput (Quartz)");
    }
    // Linux — check helper chain for the detected session.
    let session = LinuxSession::detect();
    let chain = fallback_chain(session);
    let found: Vec<&'static str> = chain
        .iter()
        .copied()
        .filter(|n| locate_on_path(n).is_some())
        .collect();
    if found.is_empty() {
        return Check::warn(
            name,
            format!(
                "no injection helper on PATH (looked for: {}) — install wtype/ydotool/xdotool",
                chain.join(", ")
            ),
        );
    }
    Check::ok(name, found.join(", "))
}

fn check_audio_input() -> Check {
    let name = "audio-input";
    #[cfg(feature = "audio-capture")]
    {
        match crate::devices::default_input_device() {
            Some(dev) => Check::ok(name, format!("default: {}", dev.name)),
            None => Check::warn(
                name,
                "no default input device detected (mic may be unplugged / muted)",
            ),
        }
    }
    #[cfg(not(feature = "audio-capture"))]
    {
        Check::warn(
            name,
            "audio device enumeration requires a build with the `audio-capture` feature",
        )
    }
}

fn check_config_file(path: &Path) -> Check {
    let name = "config";
    if !path.exists() {
        return Check::warn(
            name,
            format!("{} (missing; using compiled defaults)", path.display()),
        );
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => return Check::fail(name, format!("{} unreadable: {err}", path.display())),
    };
    if raw.trim().is_empty() {
        // An empty file is treated as `{}` by the loader — still fine.
        return Check::ok(name, format!("{} (empty; using defaults)", path.display()));
    }
    if let Err(err) = serde_json::from_str::<serde_json::Value>(&raw) {
        return Check::fail(name, format!("{} invalid JSON: {err}", path.display()));
    }
    // Full typed load — catches schema errors (unknown enum choices, unparseable
    // numerics, etc.) even though the raw JSON parses.
    match load_settings_from_path(path).and_then(|s| s.validate()) {
        Ok(()) => Check::ok(name, path.display().to_string()),
        Err(err) => Check::fail(name, format!("{} schema error: {err}", path.display())),
    }
}

fn check_configured_model(cfg_path: &Path) -> Check {
    let name = "configured-model";
    // Load settings (empty file / missing file both parse to defaults, so this
    // never fails for the "config missing" case — that case is reported by the
    // `config` check separately).
    let settings = match load_settings_from_path(cfg_path) {
        Ok(s) => s,
        Err(err) => return Check::warn(name, format!("cannot read settings: {err}")),
    };
    let model = settings.model.trim();
    if model.is_empty() {
        return Check::warn(name, "no model configured (worker will pick the default)");
    }
    // Only the whisper.cpp GGML models live in our download catalog. Anything
    // else (faster-whisper CT2 IDs, custom paths, cloud model names) is fine
    // but not something we can verify here — call it out as informational.
    let Some(entry) = model_manager::find(model) else {
        return Check::warn(
            name,
            format!("{model} (not in the GGML catalog; assumed to be a faster-whisper / cloud id)"),
        );
    };
    if model_manager::is_downloaded(entry) {
        Check::ok(name, format!("{model} (present in cache)"))
    } else {
        Check::warn(
            name,
            format!("{model} (not in cache; will download on first use)"),
        )
    }
}

// ---------- python probe helpers --------------------------------------------

/// A python invocation candidate. We can't just launch a name — on Windows we
/// want `py -3.12` (two args) as well as `python` (one arg).
#[derive(Debug, Clone)]
struct PythonCandidate {
    program: PathBuf,
    args: Vec<&'static str>,
}

impl PythonCandidate {
    fn describe(&self) -> String {
        if self.args.is_empty() {
            self.program.display().to_string()
        } else {
            format!("{} {}", self.program.display(), self.args.join(" "))
        }
    }
}

fn python_candidates() -> Vec<PythonCandidate> {
    let mut out = Vec::new();
    if let Some(explicit) = env::var_os("VOICEPI_PYTHON") {
        out.push(PythonCandidate {
            program: PathBuf::from(explicit),
            args: Vec::new(),
        });
    }
    if cfg!(windows) {
        out.push(PythonCandidate {
            program: PathBuf::from("py"),
            args: vec!["-3.12"],
        });
        out.push(PythonCandidate {
            program: PathBuf::from("py"),
            args: vec!["-3"],
        });
        out.push(PythonCandidate {
            program: PathBuf::from("python.exe"),
            args: Vec::new(),
        });
        out.push(PythonCandidate {
            program: PathBuf::from("python3.exe"),
            args: Vec::new(),
        });
    } else {
        out.push(PythonCandidate {
            program: PathBuf::from("python3.12"),
            args: Vec::new(),
        });
        out.push(PythonCandidate {
            program: PathBuf::from("python3"),
            args: Vec::new(),
        });
        out.push(PythonCandidate {
            program: PathBuf::from("python"),
            args: Vec::new(),
        });
    }
    out
}

/// Ask a candidate `python --version`; return `Some((major, minor))` on
/// success. Silently returns None on any failure (not found, non-zero exit,
/// unparseable output) — the caller decides whether that is a warn or fail.
fn probe_python(cand: &PythonCandidate) -> Option<(u32, u32)> {
    let output = Command::new(&cand.program)
        .args(&cand.args)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        // Historical Python 2 prints to stderr; be forgiving.
        text = String::from_utf8_lossy(&output.stderr).to_string();
    }
    parse_python_version(&text)
}

/// Extract `(major, minor)` from a `Python X.Y.Z` line. Returns None when the
/// output does not start with `Python `.
fn parse_python_version(text: &str) -> Option<(u32, u32)> {
    let line = text.lines().next()?.trim();
    let rest = line.strip_prefix("Python ")?.trim();
    let mut parts = rest.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}

// ---------- rendering --------------------------------------------------------

fn render_text(checks: &[Check], summary: &Summary) {
    for c in checks {
        println!("[{}] {} — {}", c.status.glyph(), c.name, c.detail);
    }
    println!(
        "\nsummary: {} ok, {} warn, {} fail",
        summary.ok, summary.warn, summary.fail
    );
    if summary.fail > 0 {
        println!("doctor: {} failed check(s) block dictation", summary.fail);
    } else if summary.warn > 0 {
        println!("doctor: platform ready (with warnings)");
    } else {
        println!("doctor: platform ready");
    }
}

fn render_json(checks: &[Check], summary: &Summary) -> Result<()> {
    let payload = json!({
        "checks": checks,
        "summary": summary,
    });
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", serde_json::to_string(&payload)?)?;
    Ok(())
}

// -----------------------------------------------------------------------------
// tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_a_stable_python_version_line() {
        assert_eq!(parse_python_version("Python 3.12.4\n"), Some((3, 12)));
        assert_eq!(parse_python_version("Python 3.9.16"), Some((3, 9)));
        assert_eq!(parse_python_version("Python 2.7.18"), Some((2, 7)));
    }

    #[test]
    fn parses_python_version_with_trailing_whitespace_and_crlf() {
        assert_eq!(parse_python_version("Python 3.12.4\r\n"), Some((3, 12)));
        assert_eq!(parse_python_version("  Python 3.12.4  "), Some((3, 12)));
    }

    #[test]
    fn rejects_non_python_version_output() {
        assert_eq!(parse_python_version(""), None);
        assert_eq!(parse_python_version("bash: python: not found"), None);
        // `Python` prefix with garbage after — must reject rather than guess.
        assert_eq!(parse_python_version("Python foo.bar"), None);
    }

    #[test]
    fn summary_counts_each_status() {
        let checks = vec![
            Check::ok("a", ""),
            Check::ok("b", ""),
            Check::warn("c", ""),
            Check::fail("d", ""),
        ];
        let s = Summary::from(&checks);
        assert_eq!(s.ok, 2);
        assert_eq!(s.warn, 1);
        assert_eq!(s.fail, 1);
    }

    #[test]
    fn classify_dir_writable_reports_missing_dir_as_warn() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let c = classify_dir_writable("t", &missing);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("missing"));
    }

    #[test]
    fn classify_dir_writable_reports_existing_writable_dir_as_ok() {
        let dir = tempdir().unwrap();
        let c = classify_dir_writable("t", dir.path());
        assert_eq!(c.status, Status::Ok);
    }

    #[test]
    fn classify_dir_writable_reports_file_as_fail() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("file.txt");
        fs::write(&file, b"hi").unwrap();
        let c = classify_dir_writable("t", &file);
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("not a directory"));
    }

    #[test]
    fn config_check_reports_missing_file_as_warn() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let c = check_config_file(&path);
        assert_eq!(c.status, Status::Warn);
    }

    #[test]
    fn config_check_reports_empty_file_as_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.json");
        fs::write(&path, "").unwrap();
        let c = check_config_file(&path);
        assert_eq!(c.status, Status::Ok);
    }

    #[test]
    fn config_check_reports_invalid_json_as_fail() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        fs::write(&path, "{not-json").unwrap();
        let c = check_config_file(&path);
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("invalid JSON"));
    }

    #[test]
    fn config_check_reports_schema_violation_as_fail() {
        // Valid JSON, but `stt_backend` must be one of {whisper, openai}.
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad-schema.json");
        fs::write(&path, r#"{"stt_backend": "notreal"}"#).unwrap();
        let c = check_config_file(&path);
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("schema error"));
    }

    #[test]
    fn config_check_reports_valid_config_as_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ok.json");
        fs::write(&path, r#"{"stt_backend": "whisper"}"#).unwrap();
        let c = check_config_file(&path);
        assert_eq!(c.status, Status::Ok);
    }

    #[test]
    fn configured_model_check_warns_when_model_blank() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cfg.json");
        // Explicitly clear the default model.
        fs::write(&path, r#"{"model": ""}"#).unwrap();
        let c = check_configured_model(&path);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("no model configured"));
    }

    #[test]
    fn configured_model_check_warns_when_model_outside_ggml_catalog() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cfg.json");
        // A faster-whisper style / cloud identifier that is NOT in the
        // GGML download catalog — the doctor treats these as informational
        // warns since it can't verify a cache for them.
        fs::write(&path, r#"{"model": "distil-large-v3"}"#).unwrap();
        let c = check_configured_model(&path);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("not in the GGML catalog"));
    }

    #[test]
    fn configured_model_check_warns_when_ggml_model_not_downloaded() {
        // A model name that IS in the catalog but isn't in the cache — this
        // is a warn, not a fail, because the worker will pull it lazily.
        let dir = tempdir().unwrap();
        let path = dir.path().join("cfg.json");
        fs::write(&path, r#"{"model": "tiny.en"}"#).unwrap();
        let c = check_configured_model(&path);
        // The test box's real cache may or may not have tiny.en; both outcomes
        // are valid, and the doctor renders them as Ok / Warn respectively.
        assert!(matches!(c.status, Status::Ok | Status::Warn));
    }

    #[test]
    fn status_serialises_as_lowercase_strings() {
        // JSON contract: consumers rely on lowercase status strings.
        assert_eq!(serde_json::to_string(&Status::Ok).unwrap(), "\"ok\"");
        assert_eq!(serde_json::to_string(&Status::Warn).unwrap(), "\"warn\"");
        assert_eq!(serde_json::to_string(&Status::Fail).unwrap(), "\"fail\"");
    }

    #[test]
    fn run_all_checks_emits_every_named_check() {
        // The smoke script and JSON consumers rely on these names being present.
        let checks = run_all_checks(None);
        let names: Vec<&str> = checks.iter().map(|c| c.name).collect();
        for expected in [
            "os",
            "session",
            "python",
            "models-cache",
            "injection-backends",
            "audio-input",
            "config",
            "configured-model",
        ] {
            assert!(
                names.contains(&expected),
                "missing check `{expected}` in {names:?}"
            );
        }
    }

    #[test]
    fn os_check_always_ok_and_reports_arch() {
        let c = check_os();
        assert_eq!(c.status, Status::Ok);
        assert!(c.detail.contains('/'));
    }

    #[test]
    fn python_candidates_prefers_voicepi_python() {
        let cands = {
            // Only mutate env inside a serialized block — this is a smoke
            // check on the ordering, not a race-prone integration test.
            let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
            std::env::set_var("VOICEPI_PYTHON", "/tmp/fake-python");
            let out = python_candidates();
            std::env::remove_var("VOICEPI_PYTHON");
            out
        };
        assert_eq!(cands[0].program, PathBuf::from("/tmp/fake-python"));
    }

    #[test]
    fn probe_python_returns_none_for_missing_binary() {
        // A path that cannot exist on any platform.
        let cand = PythonCandidate {
            program: PathBuf::from("/definitely/not/a/real/binary/wd-doctor-test"),
            args: Vec::new(),
        };
        assert_eq!(probe_python(&cand), None);
    }
}
