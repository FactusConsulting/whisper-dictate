//! Verified, cancellable download primitives for Nemotron bootstrap assets.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use super::*;
use crate::cloud_api::http::USER_AGENT;
use crate::os_cache::replace_atomic;

#[cfg(test)]
pub(super) fn download_verified(
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    target: &Path,
    label: &str,
) -> Result<()> {
    download_verified_while(
        url,
        expected_sha256,
        expected_size,
        target,
        label,
        &AtomicBool::new(true),
    )
}

pub(super) fn download_verified_while(
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    target: &Path,
    label: &str,
    runtime_active: &AtomicBool,
) -> Result<()> {
    ensure_runtime_active(runtime_active, label)?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("Nemotron {label} has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create Nemotron asset directory {}", parent.display()))?;
    if target.is_file() && verify_sha256(target, expected_sha256).is_ok() {
        return Ok(());
    }
    scavenge_stale_siblings(target, "partial", STALE_STAGING_AGE);
    let partial = unique_sibling_path(target, "partial");
    crate::diag::log!(
        "[nemotron] downloading {label} ({:.0} MB) from {url}",
        expected_size as f64 / 1_000_000.0
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        // ureq checks this deadline while `Read::read` waits for body bytes,
        // so Stop is bounded even when a peer stalls mid-download.
        .timeout_recv_body(Some(Duration::from_secs(30)))
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
            ensure_runtime_active(runtime_active, label)?;
            let count = match reader.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if !runtime_active.load(Ordering::Acquire) => {
                    return Err(anyhow!(
                        "Nemotron {label} download cancelled because the runtime stopped: {error}"
                    ));
                }
                Err(error) => return Err(error).with_context(|| format!("read Nemotron {label}")),
            };
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
        publish_verified_file(&partial, target, expected_sha256)
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

fn ensure_runtime_active(runtime_active: &AtomicBool, label: &str) -> Result<()> {
    runtime_active
        .load(Ordering::Acquire)
        .then_some(())
        .ok_or_else(|| anyhow!("Nemotron {label} download cancelled because the runtime stopped"))
}

/// Publish a fully verified file without exposing an incomplete staging path.
pub(super) fn publish_verified_file(
    partial: &Path,
    target: &Path,
    expected_sha256: &str,
) -> Result<()> {
    // GUI and CLI processes can repair the same cache entry concurrently.
    // Serialize the final verification/replacement window across processes;
    // the process-local asset mutex cannot protect this shared directory.
    let _publish_lock = acquire_asset_publish_lock(target)?;
    if target.is_file() && verify_sha256(target, expected_sha256).is_ok() {
        let _ = fs::remove_file(partial);
        return Ok(());
    }
    match fs::rename(partial, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if target.is_file() && verify_sha256(target, expected_sha256).is_ok() {
                let _ = fs::remove_file(partial);
                Ok(())
            } else {
                match replace_atomic(partial, target) {
                    Ok(()) => Ok(()),
                    Err(_)
                        if target.is_file() && verify_sha256(target, expected_sha256).is_ok() =>
                    {
                        let _ = fs::remove_file(partial);
                        Ok(())
                    }
                    Err(error) => Err(error).with_context(|| {
                        format!("replace stale Nemotron asset {}", target.display())
                    }),
                }
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn acquire_asset_publish_lock(target: &Path) -> Result<File> {
    let name = target
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("asset");
    let lock_path = target.with_file_name(format!(".{name}.publish.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open Nemotron asset lock {}", lock_path.display()))?;
    file.lock()
        .with_context(|| format!("lock Nemotron asset publication {}", lock_path.display()))?;
    Ok(file)
}

pub(super) fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    actual
        .eq_ignore_ascii_case(expected)
        .then_some(())
        .ok_or_else(|| {
            anyhow!("Nemotron cached asset SHA-256 mismatch: expected {expected}, got {actual}")
        })
}

pub(super) fn sha256_file(path: &Path) -> Result<String> {
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
    Ok(hex_lower(&hasher.finalize()))
}

pub(super) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
#[path = "nemotron_assets_download_tests.rs"]
mod tests;
