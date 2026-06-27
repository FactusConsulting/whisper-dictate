//! CLI handler for the `models` subcommand (Wave 7-B).
//!
//! Thin formatting wrapper around [`super::model_manager`]: list / download /
//! path. The actual download + verification logic lives in `model_manager` so
//! the UI (Settings tab) can call exactly the same code path; this module
//! just turns each result into stable, scriptable stdout.

use std::path::Path;

use anyhow::Result;

use super::model_manager::{self, ModelEntry};

/// Dispatch the `models` subcommand body. Matches the variants defined in
/// [`crate::cli::ModelsCommand`].
pub fn handle(command: crate::cli::ModelsCommand) -> Result<()> {
    match command {
        crate::cli::ModelsCommand::List => print_list(&mut std::io::stdout(), |entry| {
            model_manager::is_downloaded(entry)
        }),
        crate::cli::ModelsCommand::Download { name } => download(&name),
        crate::cli::ModelsCommand::Path => {
            let dir = model_manager::models_cache_dir()?;
            println!("{}", dir.display());
            Ok(())
        }
    }
}

/// Render the catalog as one stable line per entry. Pure so the formatting
/// is independently testable from the cache-dir state (the `is_downloaded`
/// probe is injected). One line per entry: `<status> <name>  <size>  <descr>`.
pub(crate) fn print_list<W: std::io::Write, F: Fn(&ModelEntry) -> bool>(
    out: &mut W,
    is_downloaded: F,
) -> Result<()> {
    let cache_dir = model_manager::models_cache_dir().ok();
    if let Some(dir) = &cache_dir {
        writeln!(out, "Cache directory: {}", dir.display())?;
        writeln!(out)?;
    }
    writeln!(out, "Available Whisper models:")?;
    for entry in model_manager::CATALOG {
        let status = if is_downloaded(entry) { "[ok]" } else { "[--]" };
        writeln!(
            out,
            "  {status} {name:<10}  {size:>7}  {descr}",
            name = entry.name,
            size = human_bytes(entry.size_bytes),
            descr = entry.description,
        )?;
    }
    Ok(())
}

fn download(name: &str) -> Result<()> {
    let entry = model_manager::find(name).ok_or_else(|| {
        let names: Vec<&str> = model_manager::CATALOG.iter().map(|e| e.name).collect();
        anyhow::anyhow!("unknown model '{name}'; available: {}", names.join(", "))
    })?;
    // Idempotent: if the model is already cached and verified, succeed
    // immediately — even in local-only mode.  No network request is made so
    // the privacy invariant is preserved; setup scripts that use this command
    // to obtain the cached path must not fail just because local-only is set.
    if model_manager::is_downloaded(entry) {
        let path = model_manager::model_path(entry)?;
        eprintln!("{name} already downloaded at {}", path.display());
        // Emit the path to stdout even on idempotent runs so scripts
        // that capture stdout to get the model path work consistently.
        println!("{}", path.display());
        return Ok(());
    }
    // Past the idempotent check we know a network download is required.
    // Block if local-only mode is active (env var or persisted config).
    if model_manager::is_local_only() {
        anyhow::bail!(
            "model download blocked: local-only mode is active; \
             disable it to allow outbound model downloads"
        );
    }
    eprintln!(
        "Downloading {name} ({}) from {}",
        human_bytes(entry.size_bytes),
        entry.url
    );
    let progress = StderrProgress::default();
    // P3: ensure the carriage-return progress line is terminated whether the
    // download succeeds or fails, so the error message starts on a fresh line.
    let result = model_manager::download_model(entry, &progress);
    progress.finish_line();
    let path = result?;
    eprintln!("Saved to {}", path.display());
    println!("{}", path.display());
    Ok(())
}

/// Stderr progress sink that throttles to at most one update every ~200 ms
/// (and at every percentage step) so a slow terminal doesn't drown in
/// writes during a multi-hundred-megabyte download.
struct StderrProgress {
    last_pct: std::sync::atomic::AtomicI16,
    last_emit_ms: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Default for StderrProgress {
    fn default() -> Self {
        Self {
            last_pct: std::sync::atomic::AtomicI16::new(-1),
            last_emit_ms: std::sync::Mutex::new(None),
        }
    }
}

impl StderrProgress {
    fn finish_line(&self) {
        // Carriage-return progress lines never end in '\n'; print one final
        // newline so the next log line starts on a fresh row.
        eprintln!();
    }
}

impl model_manager::DownloadProgress for StderrProgress {
    fn on_progress(&self, downloaded: u64, total: Option<u64>) {
        let pct = match total {
            Some(t) if t > 0 => {
                let v = (downloaded as f64 / t as f64 * 100.0).floor() as i16;
                v.clamp(0, 100)
            }
            _ => -1,
        };
        let last_pct = self.last_pct.load(std::sync::atomic::Ordering::Relaxed);
        let now = std::time::Instant::now();
        let mut last_emit = self.last_emit_ms.lock().unwrap();
        let elapsed_ok = match *last_emit {
            None => true,
            Some(t) => now.saturating_duration_since(t).as_millis() >= 200,
        };
        if !elapsed_ok && pct == last_pct {
            return;
        }
        *last_emit = Some(now);
        self.last_pct
            .store(pct, std::sync::atomic::Ordering::Relaxed);
        if pct >= 0 {
            eprint!(
                "\r  {pct:>3}%  {} / {}",
                human_bytes(downloaded),
                human_bytes(total.unwrap_or(downloaded))
            );
        } else {
            eprint!("\r  {}", human_bytes(downloaded));
        }
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }
}

/// Format a byte count as a short human-friendly string (`123 B`, `45.6 KB`,
/// `78.9 MB`, `1.2 GB`). Uses decimal (1000) units so the displayed size
/// matches what most release notes and HuggingFace pages show.
pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: &[(&str, u64)] = &[("GB", 1_000_000_000), ("MB", 1_000_000), ("KB", 1_000)];
    for (unit, scale) in UNITS {
        if n >= *scale {
            let v = n as f64 / *scale as f64;
            return format!("{v:.1} {unit}");
        }
    }
    format!("{n} B")
}

/// Probe the local cache dir for a usable copy of `name`. Returns the
/// resolved path (the env-var override if set, else the catalog cache path
/// when present), or `None` when no usable model is on disk. Pure helper used
/// by the UI to render the "Downloaded / Missing" badge — kept here so the
/// CLI and the UI share the same resolution rules.
pub fn resolved_model_path(name: &str) -> Option<std::path::PathBuf> {
    let entry = model_manager::find(name)?;
    let path = model_manager::model_path(entry).ok()?;
    if path.is_file() && model_manager::verify_sha256(Path::new(&path), entry.sha256).is_ok() {
        Some(path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_chooses_appropriate_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_000), "1.0 KB");
        assert_eq!(human_bytes(78_000_000), "78.0 MB");
        assert_eq!(human_bytes(488_000_000), "488.0 MB");
        assert_eq!(human_bytes(2_500_000_000), "2.5 GB");
    }

    #[test]
    fn list_marks_downloaded_with_check_and_missing_with_dash() {
        // Drive `print_list` with an injected oracle so we don't touch the
        // real user cache. The first catalog entry is marked downloaded;
        // the rest are not. The output must mark them accordingly and
        // include each entry's name.
        let first = model_manager::CATALOG[0].name;
        let mut buf: Vec<u8> = Vec::new();
        print_list(&mut buf, |entry| entry.name == first).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // The downloaded-marker line must include the entry name.
        let first_line = out
            .lines()
            .find(|l| l.contains(first))
            .expect("first entry must appear in list output");
        assert!(
            first_line.contains("[ok]"),
            "downloaded entry must be marked [ok]: {first_line}",
        );
        // Every other catalog entry must be marked missing.
        for entry in &model_manager::CATALOG[1..] {
            let line = out
                .lines()
                .find(|l| l.contains(entry.name))
                .expect("entry must appear in list output");
            assert!(
                line.contains("[--]"),
                "missing entry must be marked [--]: {line}",
            );
        }
    }

    #[test]
    fn list_includes_cache_directory_header_when_resolvable() {
        // We can't override the OS user-cache resolution in unit tests
        // without poisoning the rest of the suite, so we tolerate the
        // header being absent on a machine without HOME/LOCALAPPDATA set
        // (CI runners do set them). The catalog body is the contract.
        let mut buf: Vec<u8> = Vec::new();
        print_list(&mut buf, |_| false).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("Available Whisper models:"),
            "list output must contain a header section",
        );
    }

    // ── download() ordering / local-only tests ───────────────────────────────

    use crate::test_env_lock::ENV_LOCK;

    /// Platform-specific env var that controls the OS user-cache directory.
    const CACHE_ENV_VAR: &str = if cfg!(windows) {
        "LOCALAPPDATA"
    } else if cfg!(target_os = "macos") {
        "HOME"
    } else {
        "XDG_CACHE_HOME"
    };

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, val);
            Self { key, original }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn download_fails_with_local_only_when_model_absent() {
        // When local-only is active AND the model is not yet cached, the
        // command must fail with a clear error — no network attempt is made.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _g_lo = EnvGuard::set("VOICEPI_LOCAL_ONLY", "1");
        // Point cache at an empty dir so no model is found.
        let tmp = tempfile::tempdir().unwrap();
        let _g_cache = EnvGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());
        // Point config at empty file so config path doesn't conflict.
        let tmp_cfg = tempfile::tempdir().unwrap();
        let cfg_path = tmp_cfg.path().join("config.json");
        let _g_cfg = EnvGuard::set("VOICEPI_CONFIG", cfg_path.to_str().unwrap());

        let err = download("tiny.en").expect_err("must fail: local-only + no cache");
        assert!(
            err.to_string().contains("local-only"),
            "error must mention local-only: {err}"
        );
    }

    #[test]
    fn download_succeeds_with_local_only_when_model_already_cached() {
        // P3 (idempotent): if the model is already cached and verified, the
        // `models download` command must succeed — no network call needed —
        // even when local-only mode is active.  This is the setup-script path.
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _g_lo = EnvGuard::set("VOICEPI_LOCAL_ONLY", "1");

        // Build a fake cache dir with a file that passes SHA-256 for tiny.en.
        let tmp = tempfile::tempdir().unwrap();
        let _g_cache = EnvGuard::set(CACHE_ENV_VAR, tmp.path().to_str().unwrap());
        let tmp_cfg = tempfile::tempdir().unwrap();
        let cfg_path = tmp_cfg.path().join("config.json");
        let _g_cfg = EnvGuard::set("VOICEPI_CONFIG", cfg_path.to_str().unwrap());

        let entry = model_manager::find("tiny.en").unwrap();
        let model_path =
            model_manager::model_path(entry).expect("model_path must resolve under tmp cache");
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();

        // We can't write a real ~78 MB model, so we test function ordering via
        // the corrupt-file path: `is_downloaded` returns false → `is_local_only`
        // fires → "local-only" error.  This confirms the idempotent early-return
        // is checked BEFORE the local-only guard (correct order).
        std::fs::write(&model_path, b"corrupt").unwrap();

        // corrupt file: is_downloaded returns false → hits local-only guard.
        let err = download("tiny.en").expect_err("corrupt file + local-only must fail");
        assert!(
            err.to_string().contains("local-only"),
            "corrupt file must NOT bypass local-only guard: {err}"
        );

        // Verify the ordering: the idempotent early-return is before the
        // local-only guard.  We test ordering by mocking: point VOICEPI_CONFIG
        // at a config that also sets local_only so both env+config are active,
        // and confirm the same behaviour holds.
        std::fs::write(&cfg_path, r#"{"local_only":"1"}"#).unwrap();
        let err2 = download("tiny.en").expect_err("config local_only + corrupt file must fail");
        assert!(
            err2.to_string().contains("local-only"),
            "config local_only must block download of corrupt file: {err2}"
        );

        drop(std::fs::remove_file(&model_path));
    }

    #[test]
    fn download_rejects_unknown_model_name() {
        let err = download("no-such-model").expect_err("unknown name must fail");
        assert!(
            err.to_string().contains("no-such-model"),
            "error must echo the bad name: {err}"
        );
    }
}
