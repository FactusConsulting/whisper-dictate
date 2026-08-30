//! Verified extraction and atomic publication for Nemotron runtime archives.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use super::{scavenge_stale_siblings, unique_sibling_path, STALE_STAGING_AGE};

#[path = "nemotron_assets_runtime_verify.rs"]
mod verification;
#[cfg(test)]
use super::nemotron_assets_download::{sha256_file, verify_sha256_while};
use verification::runtime_cache_verified_while;
pub(super) use verification::runtime_cached_library_while;
use verification::write_runtime_verification_marker_while;
#[cfg(test)]
pub(super) use verification::{
    runtime_cache_verified, runtime_cached_library_with, write_runtime_verification_marker,
    TEST_ARCHIVE_SHA256,
};

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
    write_runtime_marker_or_cleanup(&staging, &extracted, archive_sha256, runtime_active)?;
    publish_runtime(
        &staging,
        destination,
        library_filename,
        archive_sha256,
        replace_existing,
        runtime_active,
    )
}

fn write_runtime_marker_or_cleanup(
    staging: &Path,
    library: &Path,
    archive_sha256: &str,
    runtime_active: &AtomicBool,
) -> Result<()> {
    match write_runtime_verification_marker_while(staging, library, archive_sha256, runtime_active)
    {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_dir_all(staging);
            Err(error)
        }
    }
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
    runtime_active: &AtomicBool,
) -> Result<()> {
    let result = publish_runtime_inner(
        staging,
        destination,
        library_filename,
        archive_sha256,
        replace_existing,
        runtime_active,
    );
    if result.is_err() {
        let _ = fs::remove_dir_all(staging);
    }
    result
}

fn publish_runtime_inner(
    staging: &Path,
    destination: &Path,
    library_filename: &str,
    archive_sha256: &str,
    replace_existing: bool,
    runtime_active: &AtomicBool,
) -> Result<()> {
    // The process-local asset mutex does not serialize two GUI/CLI processes.
    // Hold an OS lock across the final recheck, replacement, and rename so a
    // delayed repairer cannot delete a verified winner published by its peer.
    let _publish_lock = acquire_runtime_publish_lock(destination, runtime_active)?;
    if destination.exists() {
        if !replace_existing
            && runtime_cache_verified_while(
                destination,
                library_filename,
                archive_sha256,
                runtime_active,
            )?
        {
            let _ = fs::remove_dir_all(&staging);
            return Ok(());
        }
        fs::remove_dir_all(destination)
            .with_context(|| format!("replace old Nemotron runtime {}", destination.display()))?;
    }
    if let Err(error) = fs::rename(&staging, destination) {
        if process_winner_published_while(
            &error,
            replace_existing,
            destination,
            library_filename,
            archive_sha256,
            runtime_active,
        )? {
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
    runtime_cache_verified_while(
        destination,
        library_filename,
        archive_sha256,
        runtime_active,
    )?
    .then_some(())
    .ok_or_else(|| anyhow!("published Nemotron runtime failed verification"))
}

fn acquire_runtime_publish_lock(destination: &Path, runtime_active: &AtomicBool) -> Result<File> {
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
    loop {
        if !runtime_active.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Nemotron runtime publication cancelled because the runtime stopped"
            ));
        }
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!("lock Nemotron runtime publication {}", lock_path.display())
                });
            }
        }
    }
}

#[cfg(test)]
pub(super) fn process_winner_published(
    error: &std::io::Error,
    replace_existing: bool,
    destination: &Path,
    library_filename: &str,
    archive_sha256: &str,
) -> bool {
    process_winner_published_while(
        error,
        replace_existing,
        destination,
        library_filename,
        archive_sha256,
        &AtomicBool::new(true),
    )
    .unwrap_or(false)
}

fn process_winner_published_while(
    error: &std::io::Error,
    replace_existing: bool,
    destination: &Path,
    library_filename: &str,
    archive_sha256: &str,
    runtime_active: &AtomicBool,
) -> Result<bool> {
    Ok(!replace_existing
        && matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
        )
        && runtime_cache_verified_while(
            destination,
            library_filename,
            archive_sha256,
            runtime_active,
        )?)
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
#[path = "nemotron_assets_runtime_cancellation_tests.rs"]
mod cancellation_tests;
#[cfg(test)]
#[path = "nemotron_assets_runtime_tests.rs"]
mod tests;
