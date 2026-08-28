//! First-run asset management for the in-process Nemotron backend.
//!
//! The application binary contains the adapter, but NVIDIA's model weights and
//! native runtime remain separately licensed upstream artifacts.  This module
//! makes that distinction invisible to normal users: selecting the in-process
//! Nemotron provider resolves the official model id to a per-user cache and
//! downloads/verifies the model and NeMo-Speech.cpp runtime on first use.  An
//! explicit local path or library override still wins for developers and
//! air-gapped installations.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use crate::cloud_api::http::USER_AGENT;
use crate::dictate::backends::cloud_transcribe::{
    is_nemotron_english_model, is_nemotron_model_alias,
};
#[cfg(test)]
use crate::dictate::backends::cloud_transcribe::{NEMOTRON_ENGLISH_MODEL, NEMOTRON_MULTI_MODEL};
use crate::os_cache::{replace_atomic, user_cache_dir};

#[path = "nemotron_asset_catalog.rs"]
mod nemotron_asset_catalog;
use nemotron_asset_catalog::{
    runtime_asset, ModelAsset, RuntimeAsset, ENGLISH_MODEL, MULTI_MODEL, RUNTIME_VERSION,
};
#[cfg(test)]
use nemotron_asset_catalog::{RUNTIME_CPU, RUNTIME_VULKAN};

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
pub(crate) fn ensure_model_path(requested: &str, local_only: bool) -> Result<PathBuf> {
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
        match verify_sha256(&target, asset.sha256) {
            Ok(()) => return Ok(target),
            Err(error) => {
                // A cancelled/interrupted first-run download must not strand
                // the user with a permanently broken cache entry.  Keep the
                // failure visible in diagnostics, then fetch a clean copy.
                crate::diag::log!(
                    "[nemotron] cached model failed verification: {error}; redownloading"
                );
                fs::remove_file(&target).with_context(|| {
                    format!("remove invalid cached Nemotron model {}", target.display())
                })?;
            }
        }
    }
    if local_only {
        return Err(anyhow!(
            "Nemotron assets are missing and local-only mode blocks downloads; disable local_only or install the model manually at {}",
            target.display()
        ));
    }
    download_verified(asset.url, asset.sha256, asset.size_bytes, &target, "model")?;
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
    if let Ok(path) = super::nemotron_ffi::resolve_library_path(None) {
        if path.is_file() || super::nemotron_ffi::library_is_loadable(&path) {
            return Ok(path);
        }
    }
    let asset = runtime_asset(device);
    Ok(runtime_paths(asset)?.2)
}

pub(crate) fn ensure_library_path(
    explicit: Option<&str>,
    device: &str,
    local_only: bool,
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
    if let Ok(path) = super::nemotron_ffi::resolve_library_path(None) {
        if path.is_file() || super::nemotron_ffi::library_is_loadable(&path) {
            return Ok(path);
        }
    }
    let asset = runtime_asset(device);
    let (root, archive, library) = runtime_paths(asset)?;
    if library.is_file() {
        return Ok(library);
    }
    if local_only {
        return Err(anyhow!(
            "NeMo-Speech.cpp runtime is missing and local-only mode blocks downloads; install the {} archive beside wd.exe",
            asset.filename
        ));
    }
    download_verified(asset.url, asset.sha256, 0, &archive, "runtime archive")?;
    extract_runtime(&archive, &root, asset.library_filename)?;
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

fn download_verified(
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    target: &Path,
    label: &str,
) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("Nemotron {label} has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create Nemotron asset directory {}", parent.display()))?;
    let partial = target.with_extension("partial");
    crate::diag::log!(
        "[nemotron] downloading {label} ({:.0} MB) from {url}",
        expected_size as f64 / 1_000_000.0
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_secs(21_600)))
        .build()
        .into();
    let response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|error| anyhow!("download Nemotron {label} failed: {error}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(anyhow!("download Nemotron {label} failed: HTTP {status}"));
    }
    let (_, body) = response.into_parts();
    let mut reader = body.into_reader();
    let mut file = File::create(&partial)
        .with_context(|| format!("create Nemotron partial file {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    let result = (|| -> Result<()> {
        loop {
            let count = reader
                .read(&mut buffer)
                .with_context(|| format!("read Nemotron {label}"))?;
            if count == 0 {
                break;
            }
            file.write_all(&buffer[..count])
                .with_context(|| format!("write Nemotron {label}"))?;
            hasher.update(&buffer[..count]);
            bytes = bytes.saturating_add(count as u64);
        }
        file.flush().context("flush Nemotron asset")?;
        let actual = hex_lower(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            return Err(anyhow!(
                "Nemotron {label} SHA-256 mismatch: expected {expected_sha256}, got {actual}"
            ));
        }
        replace_atomic(&partial, target)
            .with_context(|| format!("publish Nemotron {label} {}", target.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result?;
    crate::diag::log!("[nemotron] downloaded {label} bytes={bytes}");
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = hex_lower(&hasher.finalize());
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(anyhow!(
            "Nemotron cached asset SHA-256 mismatch: expected {expected}, got {actual}"
        ))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn extract_runtime(archive: &Path, destination: &Path, library_filename: &str) -> Result<()> {
    let staging = destination.with_extension("partial");
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
        fs::remove_dir_all(destination)
            .with_context(|| format!("replace old Nemotron runtime {}", destination.display()))?;
    }
    replace_dir(&staging, destination)?;
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

fn replace_dir(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(source, destination).with_context(|| {
        format!(
            "publish Nemotron runtime {} -> {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "nemotron_assets_tests.rs"]
mod tests;
