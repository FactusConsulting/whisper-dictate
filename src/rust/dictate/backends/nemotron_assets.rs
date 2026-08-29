//! First-run asset management for the in-process Nemotron backend.
//!
//! The application binary contains the adapter, but NVIDIA's model weights and
//! native runtime remain separately licensed upstream artifacts.  This module
//! makes that distinction invisible to normal users: selecting the in-process
//! Nemotron provider resolves the official model id to a per-user cache and
//! downloads/verifies the model and NeMo-Speech.cpp runtime on first use.  An
//! explicit local path or library override still wins for developers and
//! air-gapped installations.

use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::dictate::backends::cloud_transcribe::{
    is_nemotron_english_model, is_nemotron_model_alias,
};
#[cfg(test)]
use crate::dictate::backends::cloud_transcribe::{NEMOTRON_ENGLISH_MODEL, NEMOTRON_MULTI_MODEL};
use crate::os_cache::user_cache_dir;
use anyhow::{anyhow, Context, Result};

#[path = "nemotron_asset_catalog.rs"]
mod nemotron_asset_catalog;
use nemotron_asset_catalog::{
    runtime_asset, ModelAsset, RuntimeAsset, ENGLISH_MODEL, MULTI_MODEL, RUNTIME_VERSION,
};
#[path = "nemotron_assets_download.rs"]
mod nemotron_assets_download;
#[cfg(test)]
use nemotron_asset_catalog::{RUNTIME_CPU, RUNTIME_VULKAN};
#[cfg(test)]
use nemotron_assets_download::{download_verified, hex_lower, publish_verified_file};
use nemotron_assets_download::{download_verified_while, verify_sha256};

fn cache_root() -> Result<PathBuf> {
    user_cache_dir()
        .map(|path| {
            path.join("whisper-dictate")
                .join("nemotron")
                .join(RUNTIME_VERSION)
        })
        .ok_or_else(|| anyhow!("could not resolve the user cache directory for Nemotron assets"))
}

fn model_asset(requested: &str) -> Option<ModelAsset> {
    let trimmed = requested.trim();
    if trimmed.is_empty()
        || (is_nemotron_model_alias(trimmed) && !is_nemotron_english_model(trimmed))
    {
        return Some(MULTI_MODEL);
    }
    if is_nemotron_english_model(trimmed) {
        return Some(ENGLISH_MODEL);
    }
    let filename = Path::new(trimmed)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    if filename == MULTI_MODEL.filename {
        Some(MULTI_MODEL)
    } else if filename == ENGLISH_MODEL.filename {
        Some(ENGLISH_MODEL)
    } else {
        None
    }
}

/// Return a stable cache location for a requested official model without
/// touching the network.  Runtime construction uses this planning step so a
/// first-run download never blocks the UI thread; `ensure_model_path` performs
/// the actual verified materialisation on the session worker.
pub(crate) fn model_path_for_request(requested: &str) -> Result<PathBuf> {
    let requested = requested.trim();
    if !requested.is_empty() {
        let explicit = PathBuf::from(requested);
        if explicit.is_file() {
            return Ok(explicit);
        }
    }
    let Some(asset) = model_asset(requested) else {
        let path = PathBuf::from(requested);
        return Err(anyhow!(
            "Nemotron model file does not exist: {} (use an official Nemotron model id or an existing .gguf path)",
            path.display()
        ));
    };
    Ok(cache_root()?.join("models").join(asset.filename))
}

/// Resolve a configured Nemotron model id/path. Official model ids and their
/// well-known GGUF filenames are downloaded into the user cache when missing;
/// arbitrary paths remain explicit user-managed assets.
#[cfg(test)]
pub(crate) fn ensure_model_path(requested: &str, local_only: bool) -> Result<PathBuf> {
    ensure_model_path_while(requested, local_only, &AtomicBool::new(true))
}

/// As [`ensure_model_path`], while observing the owning runtime lifecycle.
/// A Stop request must be able to abandon a first-run multi-hundred-MB model
/// transfer instead of holding the coordinator until the global HTTP timeout.
pub(crate) fn ensure_model_path_while(
    requested: &str,
    local_only: bool,
    runtime_active: &AtomicBool,
) -> Result<PathBuf> {
    let _lock = asset_lock();
    let requested = requested.trim();
    if !requested.is_empty() {
        let explicit = PathBuf::from(requested);
        if explicit.is_file() {
            return Ok(explicit);
        }
    }
    let Some(asset) = model_asset(requested) else {
        let path = PathBuf::from(requested);
        return Err(anyhow!(
            "Nemotron model file does not exist: {} (use an official Nemotron model id or an existing .gguf path)",
            path.display()
        ));
    };
    let target = cache_root()?.join("models").join(asset.filename);
    if target.is_file() {
        match verify_cached_model(&target, asset.sha256) {
            Ok(()) => return Ok(target),
            Err(error) => {
                // A cancelled/interrupted first-run download must not strand
                // the user with a permanently broken cache entry.  Keep the
                // failure visible in diagnostics, then fetch a clean copy.
                crate::diag::log!(
                    "[nemotron] cached model failed verification: {error}; redownloading"
                );
                // Do not remove it here: another GUI/CLI process can replace
                // the corrupt entry with a verified winner between our hash
                // failure and an unlink. `publish_verified_file` safely
                // replaces stale targets after our own download completes.
            }
        }
    }
    if local_only {
        return Err(anyhow!(
            "Nemotron assets are missing and local-only mode blocks downloads; disable local_only or install the model manually at {}",
            target.display()
        ));
    }
    download_verified_while(
        asset.url,
        asset.sha256,
        asset.size_bytes,
        &target,
        "model",
        runtime_active,
    )?;
    Ok(target)
}

/// Locate the native runtime beside the executable or in the cache. If no
/// override was supplied, download and extract NVIDIA's verified archive.
pub(crate) fn library_path_for_request(explicit: Option<&str>, device: &str) -> Result<PathBuf> {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "NeMo-Speech.cpp ASR library override does not exist: {}",
            path.display()
        ));
    }
    if may_reuse_discovered_runtime(device) {
        if let Ok(path) = super::nemotron_ffi::resolve_library_path(None) {
            if path.is_file() || super::nemotron_ffi::library_is_loadable(&path) {
                return Ok(path);
            }
        }
    }
    let asset = runtime_asset(device);
    Ok(runtime_paths(asset)?.2)
}

#[cfg(test)]
pub(crate) fn ensure_library_path(
    explicit: Option<&str>,
    device: &str,
    local_only: bool,
) -> Result<PathBuf> {
    ensure_library_path_while(explicit, device, local_only, &AtomicBool::new(true))
}

/// As [`ensure_library_path`], while observing the owning runtime lifecycle.
pub(crate) fn ensure_library_path_while(
    explicit: Option<&str>,
    device: &str,
    local_only: bool,
    runtime_active: &AtomicBool,
) -> Result<PathBuf> {
    let _lock = asset_lock();
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "NeMo-Speech.cpp ASR library override does not exist: {}",
            path.display()
        ));
    }
    if may_reuse_discovered_runtime(device) {
        if let Ok(path) = super::nemotron_ffi::resolve_library_path(None) {
            if path.is_file() || super::nemotron_ffi::library_is_loadable(&path) {
                return Ok(path);
            }
        }
    }
    let asset = runtime_asset(device);
    let (root, archive, library) = runtime_paths(asset)?;
    if library.is_file() {
        return Ok(library);
    }
    if local_only {
        return Err(anyhow!(
            "NeMo-Speech.cpp runtime is missing and local-only mode blocks downloads; extract {} and set VOICEPI_NEMOTRON_LIBRARY to the {} library",
            asset.filename,
            asset.library_filename,
        ));
    }
    download_verified_while(
        asset.url,
        asset.sha256,
        0,
        &archive,
        "runtime archive",
        runtime_active,
    )?;
    // A second process may have published the same runtime while this one
    // downloaded its archive. Re-check before extraction so we never replace
    // a live destination just because our process-local mutex was acquired
    // later.
    if library.is_file() {
        return Ok(library);
    }
    extract_runtime_if_missing(&archive, &root, asset.library_filename)?;
    let _ = fs::remove_file(&archive);
    if !library.is_file() {
        return Err(anyhow!(
            "NeMo-Speech.cpp archive did not contain {}",
            asset.library_filename
        ));
    }
    Ok(library)
}

fn runtime_paths(asset: RuntimeAsset) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let runtime_root = cache_root()?.join("runtime");
    let variant = asset
        .filename
        .strip_suffix(".tar.gz")
        .or_else(|| asset.filename.strip_suffix(".zip"))
        .unwrap_or(asset.filename);
    let destination = runtime_root.join(variant);
    let archive = runtime_root.join(asset.filename);
    let library = destination.join("bin").join(asset.library_filename);
    Ok((destination, archive, library))
}

fn asset_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A bundled runtime does not encode whether it was compiled for CPU, Vulkan,
/// or CUDA.  Only automatic/CPU requests can safely reuse one discovered
/// beside the executable or through the platform loader; explicit accelerators
/// must select their pinned cache variant.
fn may_reuse_discovered_runtime(device: &str) -> bool {
    matches!(
        device.trim().to_ascii_lowercase().as_str(),
        "" | "auto" | "cpu"
    )
}

/// Keep the expensive multi-hundred-MB model digest in the process-local
/// cache while still invalidating it if the on-disk file changes.  A runtime
/// unload/reload must not rehash an untouched model before every dictation.
fn verify_cached_model(path: &Path, expected_sha256: &str) -> Result<()> {
    let Some(stamp) = model_verification_stamp(path, expected_sha256) else {
        return verify_sha256(path, expected_sha256);
    };
    static VERIFIED: OnceLock<Mutex<std::collections::HashSet<ModelVerificationStamp>>> =
        OnceLock::new();
    let verified = VERIFIED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    if verified
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&stamp)
    {
        return Ok(());
    }
    verify_sha256(path, expected_sha256)?;
    verified
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(stamp);
    Ok(())
}

#[derive(Clone, Eq, PartialEq)]
struct ModelVerificationStamp {
    path: PathBuf,
    expected_sha256: String,
    bytes: u64,
    modified_ns: u128,
}

impl Hash for ModelVerificationStamp {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
        self.expected_sha256.hash(state);
        self.bytes.hash(state);
        self.modified_ns.hash(state);
    }
}

fn model_verification_stamp(path: &Path, expected_sha256: &str) -> Option<ModelVerificationStamp> {
    let metadata = fs::metadata(path).ok()?;
    let modified_ns = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(ModelVerificationStamp {
        path: path.to_path_buf(),
        expected_sha256: expected_sha256.to_ascii_lowercase(),
        bytes: metadata.len(),
        modified_ns,
    })
}

/// Return a unique sibling path for a download or extraction staging file.
/// The old deterministic `.partial` name was safe only inside one process;
/// two GUI/CLI processes could truncate each other's bytes. A per-process
/// sequence keeps every writer isolated until the verified file/directory is
/// atomically published.
fn unique_sibling_path(path: &Path, label: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("asset");
    path.with_file_name(format!(".{name}.{label}.{pid}.{id}"))
}

#[cfg(test)]
fn extract_runtime(archive: &Path, destination: &Path, library_filename: &str) -> Result<()> {
    extract_runtime_with_policy(archive, destination, library_filename, true)
}

/// Extract a verified runtime for the normal cache path. A concurrent process
/// that publishes the same variant first is the winner; this call then keeps
/// that complete destination instead of deleting/replacing a live library.
fn extract_runtime_if_missing(
    archive: &Path,
    destination: &Path,
    library_filename: &str,
) -> Result<()> {
    extract_runtime_with_policy(archive, destination, library_filename, false)
}

fn extract_runtime_with_policy(
    archive: &Path,
    destination: &Path,
    library_filename: &str,
    replace_existing: bool,
) -> Result<()> {
    let staging = unique_sibling_path(destination, "runtime-partial");
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| format!("remove {}", staging.display()))?;
    }
    fs::create_dir_all(&staging).with_context(|| format!("create {}", staging.display()))?;
    let status = if cfg!(windows) {
        let mut command = Command::new("powershell.exe");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;

            // The Rust UI is a Windows-subsystem process. Keep first-run
            // extraction invisible instead of flashing a console window over
            // the dictation surface while the runtime is unpacked.
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "Expand-Archive -LiteralPath $env:VOICEPI_NEMOTRON_ARCHIVE -DestinationPath $env:VOICEPI_NEMOTRON_DESTINATION -Force",
            ])
            .env("VOICEPI_NEMOTRON_ARCHIVE", archive)
            .env("VOICEPI_NEMOTRON_DESTINATION", &staging)
            .status()
            .context("start PowerShell to extract the Nemotron runtime")?
    } else {
        Command::new("tar")
            .args(["-xzf"])
            .arg(archive)
            .args(["-C"])
            .arg(&staging)
            .status()
            .context("start tar to extract the Nemotron runtime")?
    };
    if !status.success() {
        let _ = fs::remove_dir_all(&staging);
        return Err(anyhow!(
            "failed to extract Nemotron runtime archive (exit {})",
            status
        ));
    }
    let extracted = match find_named_file(&staging, library_filename) {
        Some(path) => path,
        None => {
            let _ = fs::remove_dir_all(&staging);
            return Err(anyhow!(
                "Nemotron runtime archive did not contain {}",
                library_filename
            ));
        }
    };
    if destination.exists() {
        if !replace_existing && find_named_file(destination, library_filename).is_some() {
            // Another process completed the same verified runtime while this
            // extraction was in progress. Its complete destination wins.
            let _ = fs::remove_dir_all(&staging);
            return Ok(());
        }
        fs::remove_dir_all(destination)
            .with_context(|| format!("replace old Nemotron runtime {}", destination.display()))?;
    }
    if let Err(error) = fs::rename(&staging, destination) {
        if process_winner_published(&error, replace_existing, destination, library_filename) {
            let _ = fs::remove_dir_all(&staging);
            return Ok(());
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error).with_context(|| {
            format!(
                "publish Nemotron runtime {} -> {}",
                staging.display(),
                destination.display()
            )
        });
    }
    let published = find_named_file(destination, library_filename).ok_or_else(|| {
        anyhow!(
            "Nemotron runtime extraction lost {} (source was {})",
            library_filename,
            extracted.display()
        )
    })?;
    if !published.is_file() {
        return Err(anyhow!("published Nemotron library is not a file"));
    }
    Ok(())
}

fn process_winner_published(
    error: &std::io::Error,
    replace_existing: bool,
    destination: &Path,
    library_filename: &str,
) -> bool {
    !replace_existing
        && matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
        )
        && find_named_file(destination, library_filename).is_some()
}

fn find_named_file(root: &Path, filename: &str) -> Option<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let entries = fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_file() && child.file_name().and_then(|n| n.to_str()) == Some(filename) {
                return Some(child);
            }
            if child.is_dir() {
                pending.push(child);
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "nemotron_assets_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "nemotron_assets_runtime_tests.rs"]
mod runtime_tests;
