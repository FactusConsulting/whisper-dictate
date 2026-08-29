//! Verified extraction and atomic publication for Nemotron runtime archives.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    nemotron_assets_download::{sha256_file, verify_sha256},
    scavenge_stale_siblings, unique_sibling_path, STALE_STAGING_AGE,
};

const VERIFICATION_MARKER: &str = ".whisper-dictate-runtime-sha256";
#[cfg(test)]
pub(super) const TEST_ARCHIVE_SHA256: &str = "test-archive-sha256";

#[derive(Debug, Deserialize, Serialize)]
struct RuntimeVerificationManifest {
    archive: String,
    library: String,
    files: BTreeMap<String, String>,
}

pub(super) fn runtime_cache_verified(
    destination: &Path,
    library_filename: &str,
    expected_archive_sha256: &str,
) -> bool {
    let Ok(marker) = fs::read(destination.join(VERIFICATION_MARKER)) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<RuntimeVerificationManifest>(&marker) else {
        return false;
    };
    if !manifest
        .archive
        .eq_ignore_ascii_case(expected_archive_sha256)
        || manifest.files.is_empty()
    {
        return false;
    }
    let Some(library) = manifest_path(destination, &manifest.library) else {
        return false;
    };
    if library.file_name().and_then(std::ffi::OsStr::to_str) != Some(library_filename)
        || !manifest.files.contains_key(&manifest.library)
    {
        return false;
    }
    manifest.files.iter().all(|(relative, sha256)| {
        manifest_path(destination, relative)
            .is_some_and(|path| verify_sha256(&path, sha256).is_ok())
    })
}

pub(super) fn write_runtime_verification_marker(
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

#[cfg(test)]
pub(super) fn extract_runtime(
    archive: &Path,
    destination: &Path,
    library_filename: &str,
) -> Result<()> {
    extract_runtime_with_policy(
        archive,
        destination,
        library_filename,
        TEST_ARCHIVE_SHA256,
        true,
        &AtomicBool::new(true),
    )
}

/// Extract a verified runtime for the normal cache path. A concurrent process
/// that publishes the same archive first is the winner; incomplete or
/// unverified cache directories are replaced.
pub(super) fn extract_runtime_if_missing(
    archive: &Path,
    destination: &Path,
    library_filename: &str,
    archive_sha256: &str,
    runtime_active: &AtomicBool,
) -> Result<()> {
    extract_runtime_with_policy(
        archive,
        destination,
        library_filename,
        archive_sha256,
        false,
        runtime_active,
    )
}

fn extract_runtime_with_policy(
    archive: &Path,
    destination: &Path,
    library_filename: &str,
    archive_sha256: &str,
    replace_existing: bool,
    runtime_active: &AtomicBool,
) -> Result<()> {
    scavenge_stale_siblings(destination, "runtime-partial", STALE_STAGING_AGE);
    let staging = unique_sibling_path(destination, "runtime-partial");
    fs::create_dir_all(&staging).with_context(|| format!("create {}", staging.display()))?;
    let status_result = if cfg!(windows) {
        let mut command = Command::new("powershell.exe");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        command.args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "Expand-Archive -LiteralPath $env:VOICEPI_NEMOTRON_ARCHIVE -DestinationPath $env:VOICEPI_NEMOTRON_DESTINATION -Force",
            ])
            .env("VOICEPI_NEMOTRON_ARCHIVE", archive)
            .env("VOICEPI_NEMOTRON_DESTINATION", &staging);
        run_extraction_command(&mut command, runtime_active)
    } else {
        let mut command = Command::new("tar");
        command
            .args(["-xzf"])
            .arg(archive)
            .args(["-C"])
            .arg(&staging);
        run_extraction_command(&mut command, runtime_active)
    };
    let status = match status_result {
        Ok(status) => status,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
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
    write_runtime_verification_marker(&staging, &extracted, archive_sha256)?;
    publish_runtime(
        &staging,
        destination,
        library_filename,
        archive_sha256,
        replace_existing,
    )
}

fn run_extraction_command(
    command: &mut Command,
    runtime_active: &AtomicBool,
) -> Result<ExitStatus> {
    if !runtime_active.load(Ordering::Acquire) {
        return Err(anyhow!(
            "Nemotron runtime extraction cancelled because the runtime stopped"
        ));
    }
    let mut child = command
        .spawn()
        .context("start process to extract the Nemotron runtime")?;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("wait for Nemotron runtime extraction")?
        {
            return Ok(status);
        }
        if !runtime_active.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "Nemotron runtime extraction cancelled because the runtime stopped"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn publish_runtime(
    staging: &Path,
    destination: &Path,
    library_filename: &str,
    archive_sha256: &str,
    replace_existing: bool,
) -> Result<()> {
    // The process-local asset mutex does not serialize two GUI/CLI processes.
    // Hold an OS lock across the final recheck, replacement, and rename so a
    // delayed repairer cannot delete a verified winner published by its peer.
    let _publish_lock = acquire_runtime_publish_lock(destination)?;
    if destination.exists() {
        if !replace_existing
            && runtime_cache_verified(destination, library_filename, archive_sha256)
        {
            let _ = fs::remove_dir_all(&staging);
            return Ok(());
        }
        fs::remove_dir_all(destination)
            .with_context(|| format!("replace old Nemotron runtime {}", destination.display()))?;
    }
    if let Err(error) = fs::rename(&staging, destination) {
        if process_winner_published(
            &error,
            replace_existing,
            destination,
            library_filename,
            archive_sha256,
        ) {
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
    runtime_cache_verified(destination, library_filename, archive_sha256)
        .then_some(())
        .ok_or_else(|| anyhow!("published Nemotron runtime failed verification"))
}

fn acquire_runtime_publish_lock(destination: &Path) -> Result<File> {
    let name = destination
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("runtime");
    let lock_path = destination.with_file_name(format!(".{name}.publish.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open Nemotron runtime lock {}", lock_path.display()))?;
    file.lock()
        .with_context(|| format!("lock Nemotron runtime publication {}", lock_path.display()))?;
    Ok(file)
}

pub(super) fn process_winner_published(
    error: &std::io::Error,
    replace_existing: bool,
    destination: &Path,
    library_filename: &str,
    archive_sha256: &str,
) -> bool {
    !replace_existing
        && matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
        )
        && runtime_cache_verified(destination, library_filename, archive_sha256)
}

pub(super) fn find_named_file(root: &Path, filename: &str) -> Option<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let entries = fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_file()
                && child.file_name().and_then(std::ffi::OsStr::to_str) == Some(filename)
            {
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
#[path = "nemotron_assets_runtime_tests.rs"]
mod tests;
