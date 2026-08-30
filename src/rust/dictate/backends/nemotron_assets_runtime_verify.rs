//! Manifest parsing, integrity verification, and process-local identity cache
//! for an extracted Nemotron runtime.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::super::nemotron_assets_download::{sha256_file, verify_sha256_while};

const VERIFICATION_MARKER: &str = ".whisper-dictate-runtime-sha256";
#[cfg(test)]
pub(crate) const TEST_ARCHIVE_SHA256: &str = "test-archive-sha256";

#[derive(Debug, Deserialize, Serialize)]
struct RuntimeVerificationManifest {
    archive: String,
    library: String,
    files: BTreeMap<String, String>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct RuntimeVerificationStamp {
    destination: PathBuf,
    expected_archive_sha256: String,
    marker: Vec<u8>,
    files: Vec<RuntimeFileStamp>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct RuntimeFileStamp {
    relative: String,
    bytes: u64,
    modified_ns: u128,
}

#[cfg(test)]
pub(crate) fn runtime_cache_verified(
    destination: &Path,
    library_filename: &str,
    expected_archive_sha256: &str,
) -> bool {
    runtime_cache_verified_while(
        destination,
        library_filename,
        expected_archive_sha256,
        &AtomicBool::new(true),
    )
    .unwrap_or(false)
}

pub(super) fn runtime_cache_verified_while(
    destination: &Path,
    library_filename: &str,
    expected_archive_sha256: &str,
    runtime_active: &AtomicBool,
) -> Result<bool> {
    Ok(runtime_cached_library_while(
        destination,
        library_filename,
        expected_archive_sha256,
        runtime_active,
    )?
    .is_some())
}

pub(crate) fn runtime_cached_library_while(
    destination: &Path,
    library_filename: &str,
    expected_archive_sha256: &str,
    runtime_active: &AtomicBool,
) -> Result<Option<PathBuf>> {
    runtime_cached_library_with_impl(
        destination,
        library_filename,
        expected_archive_sha256,
        runtime_active,
        verify_sha256_while,
    )
}

#[cfg(test)]
pub(crate) fn runtime_cached_library_with<F>(
    destination: &Path,
    library_filename: &str,
    expected_archive_sha256: &str,
    runtime_active: &AtomicBool,
    verify: F,
) -> Result<Option<PathBuf>>
where
    F: FnMut(&Path, &str, &AtomicBool, &str) -> Result<()>,
{
    runtime_cached_library_with_impl(
        destination,
        library_filename,
        expected_archive_sha256,
        runtime_active,
        verify,
    )
}

fn runtime_cached_library_with_impl<F>(
    destination: &Path,
    library_filename: &str,
    expected_archive_sha256: &str,
    runtime_active: &AtomicBool,
    mut verify: F,
) -> Result<Option<PathBuf>>
where
    F: FnMut(&Path, &str, &AtomicBool, &str) -> Result<()>,
{
    if !runtime_active.load(Ordering::Acquire) {
        return Err(anyhow!(
            "Nemotron runtime cache verification cancelled because the runtime stopped"
        ));
    }
    let Ok(marker) = fs::read(destination.join(VERIFICATION_MARKER)) else {
        return Ok(None);
    };
    let Ok(manifest) = serde_json::from_slice::<RuntimeVerificationManifest>(&marker) else {
        return Ok(None);
    };
    if !manifest
        .archive
        .eq_ignore_ascii_case(expected_archive_sha256)
        || manifest.files.is_empty()
    {
        return Ok(None);
    }
    let Some(library) = manifest_path(destination, &manifest.library) else {
        return Ok(None);
    };
    if library.file_name().and_then(std::ffi::OsStr::to_str) != Some(library_filename)
        || !manifest.files.contains_key(&manifest.library)
    {
        return Ok(None);
    }
    let Some(stamp) = runtime_verification_stamp(
        destination,
        expected_archive_sha256,
        &marker,
        &manifest,
        runtime_active,
    )?
    else {
        return Ok(None);
    };
    static VERIFIED: OnceLock<Mutex<HashSet<RuntimeVerificationStamp>>> = OnceLock::new();
    let verified = VERIFIED.get_or_init(|| Mutex::new(HashSet::new()));
    if verified
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&stamp)
    {
        return Ok(Some(library));
    }
    for (relative, sha256) in &manifest.files {
        let Some(path) = manifest_path(destination, relative) else {
            return Ok(None);
        };
        match verify(&path, sha256, runtime_active, "runtime cache file") {
            Ok(()) => {}
            Err(error) if !runtime_active.load(Ordering::Acquire) => return Err(error),
            Err(_) => return Ok(None),
        }
    }
    let Ok(marker_after) = fs::read(destination.join(VERIFICATION_MARKER)) else {
        return Ok(None);
    };
    let Some(stamp_after) = runtime_verification_stamp(
        destination,
        expected_archive_sha256,
        &marker_after,
        &manifest,
        runtime_active,
    )?
    else {
        return Ok(None);
    };
    if stamp_after != stamp {
        return Ok(None);
    }
    verified
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(stamp);
    Ok(Some(library))
}

fn runtime_verification_stamp(
    destination: &Path,
    expected_archive_sha256: &str,
    marker: &[u8],
    manifest: &RuntimeVerificationManifest,
    runtime_active: &AtomicBool,
) -> Result<Option<RuntimeVerificationStamp>> {
    let mut files = Vec::with_capacity(manifest.files.len());
    for relative in manifest.files.keys() {
        if !runtime_active.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Nemotron runtime cache verification cancelled because the runtime stopped"
            ));
        }
        let Some(path) = manifest_path(destination, relative) else {
            return Ok(None);
        };
        let Ok(metadata) = fs::metadata(path) else {
            return Ok(None);
        };
        if !metadata.is_file() {
            return Ok(None);
        }
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_nanos());
        files.push(RuntimeFileStamp {
            relative: relative.clone(),
            bytes: metadata.len(),
            modified_ns,
        });
    }
    Ok(Some(RuntimeVerificationStamp {
        destination: destination.to_path_buf(),
        expected_archive_sha256: expected_archive_sha256.to_ascii_lowercase(),
        marker: marker.to_vec(),
        files,
    }))
}

pub(crate) fn write_runtime_verification_marker(
    destination: &Path,
    library: &Path,
    archive_sha256: &str,
) -> Result<()> {
    let library = library
        .strip_prefix(destination)
        .with_context(|| format!("locate {} inside runtime cache", library.display()))?;
    let library = manifest_relative_path(library)?;
    let files = runtime_file_hashes(destination)?;
    if !files.contains_key(&library) {
        return Err(anyhow!(
            "Nemotron runtime verification manifest omitted {}",
            library
        ));
    }
    let marker = serde_json::to_vec(&RuntimeVerificationManifest {
        archive: archive_sha256.to_owned(),
        library,
        files,
    })?;
    fs::write(destination.join(VERIFICATION_MARKER), marker).with_context(|| {
        format!(
            "write Nemotron runtime verification marker in {}",
            destination.display()
        )
    })
}

fn runtime_file_hashes(destination: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![destination.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read Nemotron runtime directory {}", directory.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.is_file()
                && path.file_name().and_then(std::ffi::OsStr::to_str) != Some(VERIFICATION_MARKER)
            {
                let relative = path.strip_prefix(destination).with_context(|| {
                    format!("relativize Nemotron runtime file {}", path.display())
                })?;
                files.insert(manifest_relative_path(relative)?, sha256_file(&path)?);
            }
        }
    }
    Ok(files)
}

fn manifest_relative_path(path: &Path) -> Result<String> {
    let parts = path
        .components()
        .map(|component| match component {
            std::path::Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("Nemotron runtime path is not valid UTF-8")),
            _ => Err(anyhow!("Nemotron runtime manifest path is not relative")),
        })
        .collect::<Result<Vec<_>>>()?;
    (!parts.is_empty())
        .then(|| parts.join("/"))
        .ok_or_else(|| anyhow!("Nemotron runtime manifest path is empty"))
}

fn manifest_path(destination: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = destination.to_path_buf();
    let mut saw_component = false;
    for component in relative.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return None;
        }
        saw_component = true;
        path.push(component);
    }
    saw_component.then_some(path)
}
