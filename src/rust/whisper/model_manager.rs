//! Catalog + download/verify/cache for whisper.cpp GGML models.
//!
//! Wave 7-B of the Python-removal roadmap (#348, sub-task #317). Until now the
//! local Whisper model was supplied only via `VOICEPI_WHISPER_MODEL_PATH`: the
//! user had to download a `ggml-*.bin` file by hand and point the env var at
//! it. This module gives the app a self-contained way to fetch a known model
//! file, verify its SHA-256 against an in-binary catalog, and store it under
//! the OS user-cache directory. The env-var path keeps working untouched for
//! power users who already have a custom GGML file (or want a model that isn't
//! in the curated catalog).
//!
//! Scope is intentionally minimal:
//! - **Catalog**: tiny.en / base.en / small.en (CPU-friendly English models).
//!   Larger or multilingual models are explicitly out of scope for the first
//!   cut — power users extend via the env var.
//! - **Format**: GGML only. whisper.cpp does not yet read GGUF (see the
//!   `reject_gguf_model` guard in `whisper::local`), so a GGUF entry in the
//!   catalog would only mislead.
//! - **Transport**: blocking `ureq` streaming download. No async runtime, no
//!   tokio. Progress is reported via a callback the UI / CLI provides.
//! - **Verification**: SHA-256 computed during the streaming write, checked
//!   AFTER the file is on disk. A mismatch deletes the partial download so the
//!   next attempt starts clean.
//! - **Atomicity**: writes to `<name>.partial` first, renames into place on
//!   verified success (Windows-friendly via `replace_atomic` style — see
//!   `audio::model_cache` for the same pattern).
//!
//! This module is compiled **unconditionally** (no `whisper-rs-local` feature
//! gate) so the CLI `models list` / `models download` subcommands and the UI
//! Settings tab section work on every binary — even one built without the
//! whisper.cpp inference path. The actual model load (`LocalWhisper::new`)
//! still requires the feature.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

/// One entry in the curated download catalog.
///
/// The fields are deliberately `&'static str` / `u64` (no allocation) so the
/// catalog is a compile-time constant. The `sha256` value is the canonical
/// hex-lowercase digest of the file at `url`; the in-binary value is the
/// source of truth for integrity checks. URLs point at the upstream
/// `ggerganov/whisper.cpp` HuggingFace mirror — the same source the upstream
/// `models/download-ggml-model.sh` script uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelEntry {
    /// Short stable name shown in the UI and accepted by `models download`
    /// (for example `"tiny.en"`). Must be unique within the catalog.
    pub name: &'static str,
    /// On-disk filename used inside the cache directory (matches the
    /// upstream `ggml-*.bin` naming so a downloaded file is byte-identical
    /// to one fetched with the upstream script and can be moved between
    /// installs).
    pub filename: &'static str,
    /// HTTPS URL of the GGML weights file.
    pub url: &'static str,
    /// Hex-lowercase SHA-256 of the file at `url`. Verified after every
    /// download and exposed via `verify_sha256` for manual integrity checks
    /// against an already-cached or env-var-supplied file.
    pub sha256: &'static str,
    /// Approximate on-disk size in bytes, used by the UI to size the
    /// progress bar before the HTTP response carries a `Content-Length`
    /// header. Not authoritative — the actual `Content-Length` (when
    /// available) and final byte count win.
    pub size_bytes: u64,
    /// One-line human-readable summary for the UI label
    /// (accuracy / speed / file size tradeoff).
    pub description: &'static str,
}

/// Curated catalog of CPU-friendly English GGML models. Order matches the UI
/// presentation: smallest / fastest first so the cheapest option is the
/// default eye-line pick. SHA-256 values are pinned to the current
/// `ggerganov/whisper.cpp` HuggingFace main branch — re-verify when bumping.
pub const CATALOG: &[ModelEntry] = &[
    ModelEntry {
        name: "tiny.en",
        filename: "ggml-tiny.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
        size_bytes: 77_700_000,
        description: "English, fastest, lowest accuracy (~78 MB)",
    },
    ModelEntry {
        name: "base.en",
        filename: "ggml-base.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
        size_bytes: 147_900_000,
        description: "English, fast, low accuracy (~148 MB)",
    },
    ModelEntry {
        name: "small.en",
        filename: "ggml-small.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        size_bytes: 487_600_000,
        description: "English, balanced accuracy & speed (~488 MB)",
    },
];

/// Look up a catalog entry by its short name.
pub fn find(name: &str) -> Option<&'static ModelEntry> {
    CATALOG.iter().find(|entry| entry.name == name)
}

/// Resolve the OS-conventional user-cache subdirectory we store Whisper
/// models in.
///
/// Mirrors `audio::model_cache::user_cache_dir` (`%LOCALAPPDATA%` on Windows,
/// `~/Library/Caches` on macOS, `$XDG_CACHE_HOME`/`~/.cache` on Linux) and
/// nests one extra `whisper-models/` segment under the shared
/// `whisper-dictate/` namespace so the bundled Silero VAD model and the
/// per-user Whisper downloads don't collide.
pub fn models_cache_dir() -> Result<PathBuf> {
    let base = user_cache_dir().ok_or_else(|| {
        anyhow!(
            "could not resolve the user cache directory; set HOME / LOCALAPPDATA \
             / XDG_CACHE_HOME to point at a writable location"
        )
    })?;
    Ok(base.join("whisper-dictate").join("whisper-models"))
}

/// Path the given catalog entry would be written to once downloaded.
///
/// Note: existence of the file at this path does NOT imply it is a
/// complete / verified download — use [`is_downloaded`] for that. We never
/// publish a path until the SHA-256 check passes (see `download_model`), so
/// in practice `exists(path) == is_downloaded(entry)`, but the explicit
/// check is cheap and protects against an externally truncated file.
pub fn model_path(entry: &ModelEntry) -> Result<PathBuf> {
    Ok(models_cache_dir()?.join(entry.filename))
}

/// True iff the cached file for `entry` exists on disk AND its SHA-256
/// matches the catalog. A partial / corrupt file reads as "not downloaded"
/// so the UI offers a fresh download instead of a confusing "ready" badge.
pub fn is_downloaded(entry: &ModelEntry) -> bool {
    let Ok(path) = model_path(entry) else {
        return false;
    };
    if !path.is_file() {
        return false;
    }
    verify_sha256(&path, entry.sha256).is_ok()
}

/// Hash the file at `path` and check it equals `expected_hex`. Returns
/// `Ok(())` on match; an error with both the expected and actual digests on
/// mismatch (so a logged-out failure tells the user which one is wrong, not
/// just "hash didn't match"). Streams the file in 64 KiB chunks so a
/// hundreds-of-MB model doesn't go through the heap.
pub fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    let expected = normalize_hex(expected_hex);
    let mut f = File::open(path)
        .with_context(|| format!("failed to open {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = hex_lower(&hasher.finalize());
    if actual != expected {
        return Err(anyhow!(
            "SHA-256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        ));
    }
    Ok(())
}

/// Returns `true` when local-only mode is active. Checks two sources in order:
///
/// 1. `VOICEPI_LOCAL_ONLY` environment variable (`"1"`, `"true"`, `"True"`,
///    or `"TRUE"`) — fast, process-level override.
/// 2. Persisted config `local_only: true` in `settings.json` — covers the
///    common case where the user toggled the Settings switch and saved but did
///    NOT export the env var in the calling shell (Privacy bug P1 fix).
///
/// Used to gate outbound model downloads; callers that would initiate network
/// requests must check this before proceeding.
pub fn is_local_only() -> bool {
    // Fast path: env var takes precedence and avoids a config-file read.
    if std::env::var("VOICEPI_LOCAL_ONLY")
        .map(|v| matches!(v.trim(), "1" | "true" | "True" | "TRUE"))
        .unwrap_or(false)
    {
        return true;
    }
    // Fallback: persisted config setting.  `load_settings` reads from disk
    // only at download time (not on every frame) so the cost is acceptable.
    crate::config::load_settings()
        .map(|s| s.local_only)
        .unwrap_or(false)
}

/// Callback the download path invokes as bytes land. Implemented by the UI
/// (Arc<Mutex<...>> over a shared progress struct) and by the CLI (writes a
/// percentage line to stderr). `total` is the `Content-Length` value when the
/// server provided one — and `None` otherwise — so the receiver can either
/// render a real bar or a rolling byte counter.
pub trait DownloadProgress: Send + Sync {
    fn on_progress(&self, downloaded: u64, total: Option<u64>);
}

/// `()` is a no-op progress sink: handy for tests and for the
/// `is_downloaded` path that doesn't need any reporting.
impl DownloadProgress for () {
    fn on_progress(&self, _downloaded: u64, _total: Option<u64>) {}
}

/// TCP connect timeout for model downloads.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// End-to-end timeout for model downloads (default 60 minutes).
///
/// At 0.5 Mbit/s the models require roughly:
///   tiny.en  (~78 MB)  → ~21 min
///   base.en (~148 MB)  → ~39 min
///   small.en (~488 MB) → ~130 min
///
/// The default 60-minute cap covers tiny.en and base.en on very slow
/// connections. Users on slower links or downloading small.en can override
/// via `VOICEPI_MODEL_DOWNLOAD_TIMEOUT_SECS`.
const DEFAULT_DOWNLOAD_TIMEOUT_SECS: u64 = 3_600;

fn download_timeout() -> std::time::Duration {
    let secs = std::env::var("VOICEPI_MODEL_DOWNLOAD_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DOWNLOAD_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Download `entry` to its cache path, streaming via `ureq` and reporting
/// progress through `cb`. Returns the final on-disk path on success.
///
/// The download writes to `<filename>.partial` first; on successful SHA-256
/// verification the partial is atomically renamed into the real cache path.
/// On any failure the partial is removed so the next attempt starts clean.
///
/// Errors immediately when local-only mode is active (either the
/// `VOICEPI_LOCAL_ONLY` env var or the persisted `local_only` config setting);
/// the caller must gate its UI affordances on `is_local_only()` as well for a
/// consistent UX.
pub fn download_model(entry: &ModelEntry, cb: &dyn DownloadProgress) -> Result<PathBuf> {
    if is_local_only() {
        return Err(anyhow!(
            "model download blocked: local-only mode is active \
             (VOICEPI_LOCAL_ONLY env var or local_only setting); \
             disable local-only mode to allow outbound model downloads"
        ));
    }
    let target = model_path(entry)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create model cache directory {}",
                parent.display()
            )
        })?;
    }
    let partial = partial_path(&target);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(download_timeout()))
        .build()
        .into();
    let response = agent
        .get(entry.url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|err| anyhow!("download request failed: {err}"))?;
    let content_length = response
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let (_, body) = response.into_parts();
    let mut reader = body.into_reader();
    stream_download_to(
        &mut reader,
        content_length,
        &partial,
        &target,
        entry.sha256,
        cb,
    )?;
    Ok(target)
}

/// Pure I/O core extracted from `download_model` so tests can drive it with
/// any `Read` (file, `Cursor`, mock) without touching the network. Hashes +
/// reports progress while writing, then verifies the on-disk file and
/// renames into place.
pub(crate) fn stream_download_to<R: Read>(
    reader: &mut R,
    total: Option<u64>,
    partial: &Path,
    target: &Path,
    expected_sha256: &str,
    cb: &dyn DownloadProgress,
) -> Result<()> {
    // Initial "started" tick so a UI bar shows movement before the first
    // chunk lands (DNS + TLS handshake can take a noticeable beat on cold
    // networks).
    cb.on_progress(0, total);
    let mut file =
        File::create(partial).with_context(|| format!("failed to create {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(err) => {
                drop(file);
                let _ = fs::remove_file(partial);
                return Err(anyhow!("download read failed: {err}"));
            }
        };
        if n == 0 {
            break;
        }
        if let Err(err) = file.write_all(&buf[..n]) {
            drop(file);
            let _ = fs::remove_file(partial);
            return Err(anyhow!("failed to write {}: {err}", partial.display()));
        }
        hasher.update(&buf[..n]);
        downloaded = downloaded.saturating_add(n as u64);
        cb.on_progress(downloaded, total);
    }
    if let Err(err) = file.flush() {
        drop(file);
        let _ = fs::remove_file(partial);
        return Err(anyhow!("failed to flush {}: {err}", partial.display()));
    }
    drop(file);

    let actual = hex_lower(&hasher.finalize());
    let expected = normalize_hex(expected_sha256);
    if actual != expected {
        let _ = fs::remove_file(partial);
        return Err(anyhow!(
            "SHA-256 mismatch after download: expected {}, got {}",
            expected,
            actual
        ));
    }
    if let Err(err) = replace_atomic(partial, target) {
        let _ = fs::remove_file(partial);
        return Err(anyhow!(
            "failed to install {} into {}: {err}",
            partial.display(),
            target.display()
        ));
    }
    Ok(())
}

/// Return a per-process, per-call temporary path for a partial download.
///
/// Uses `<target>.<pid>-<seq>.partial` (process-ID + monotonic sequence) so
/// two simultaneous invocations (CLI + UI, or two CLI calls) never collide on
/// the same partial file. The `.partial` suffix is stable so cleanup scripts
/// can still glob for stale partials.
fn partial_path(target: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let suffix = format!(".{pid}-{seq}.partial");
    let mut s = target.as_os_str().to_owned();
    s.push(suffix.as_str());
    PathBuf::from(s)
}

/// Cross-platform "rename, replacing the destination if it exists". Same
/// dance `audio::model_cache::replace_atomic` does — on POSIX `rename` is
/// atomic and overwrites; on Windows we delete-then-rename on the
/// `AlreadyExists` case only (so unrelated errors surface untouched).
fn replace_atomic(tmp: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        match fs::rename(tmp, target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(target);
                fs::rename(tmp, target)
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(tmp, target)
    }
}

/// Resolve the OS-conventional user cache directory. Duplicated from
/// `audio::model_cache` so this module doesn't introduce a cross-module
/// dependency on a feature-gated module (`audio` only compiles with
/// `audio-in-rust`). The rules match `dirs::cache_dir` for our three
/// platforms.
fn user_cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            return Some(PathBuf::from(xdg));
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Lowercase + trim hex input so a catalog typo in casing doesn't fail
/// verification of an otherwise-correct file. Whitespace around env-supplied
/// values is also stripped.
fn normalize_hex(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

const USER_AGENT: &str = concat!(
    "whisper-dictate-model-fetch/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/FactusConsulting/whisper-dictate)",
);

#[cfg(test)]
#[path = "model_manager_tests.rs"]
mod tests;
