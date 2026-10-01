//! Explicit opt-in pruning; bounded row buffers, one recovery generation.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::Value;

use crate::{atomic_file, jsonl_file};

const MAX_LINE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ENTRIES: usize = 100_000;

pub(crate) fn parse_limit(value: &str) -> usize {
    value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|limit| *limit <= MAX_ENTRIES)
        .unwrap_or(0)
}

/// Present invalid config must disable pruning, not inherit an ambient cap.
pub(crate) fn parse_config_limit(value: &Value) -> usize {
    match value {
        Value::String(value) => parse_limit(value),
        Value::Number(value) => value
            .as_u64()
            .filter(|limit| *limit <= MAX_ENTRIES as u64)
            .map(|limit| limit as usize)
            .unwrap_or(0),
        _ => 0,
    }
}

pub(crate) fn append(
    path: &Path,
    event: &Value,
    limit: usize,
    metrics_path: Option<&Path>,
) -> io::Result<()> {
    append_with_support(
        path,
        event,
        limit,
        metrics_path,
        cfg!(any(windows, target_os = "linux")),
    )
}

fn append_with_support(
    path: &Path,
    event: &Value,
    limit: usize,
    metrics_path: Option<&Path>,
    retention_supported: bool,
) -> io::Result<()> {
    if limit > MAX_ENTRIES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history retention limit is out of range",
        ));
    }
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    if limit > 0 && line.len() > MAX_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "history row exceeds retention safety limit",
        ));
    }
    let locked = jsonl_file::acquire(path)?;
    if limit == 0 || !retention_supported {
        if limit > 0 {
            separate_final_row(&locked.path, &mut line)?;
        }
        let result = jsonl_file::append_locked(&locked.path, &line);
        drop(locked);
        result?;
        if limit > 0 {
            crate::diag::log!(
                "[history] retention unavailable on this platform; appended without pruning"
            );
        }
        return Ok(());
    }
    let backup = jsonl_file::sibling(&locked.path, ".retention-backup")?;
    if let Some(metrics) = metrics_path {
        // A missing metrics parent need not exist yet; exact normalized paths
        // catch configured aliases where possible, and unreadable identities
        // fail closed rather than guessing that pruning would be safe.
        let metrics = metrics_identity(metrics)?;
        if jsonl_file::same_identity(&metrics, &locked.path)?
            || jsonl_file::same_identity(&metrics, &backup)?
        {
            return Err(io::Error::other(
                "history retention requires a separate file from metrics",
            ));
        }
    }
    let (rows, final_newline) = match inspect(&locked.path) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            separate_final_row(&locked.path, &mut line)?;
            let result = jsonl_file::append_locked(&locked.path, &line);
            // Diagnostics can share this destination; release its lock first.
            drop(locked);
            result?;
            crate::diag::log!(
                "[history] retention skipped: invalid old rows; appended without pruning; \
                 repair old rows to resume retention"
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if rows < limit {
        if rows > 0 && !final_newline {
            line.insert(0, b'\n');
        }
        return jsonl_file::append_locked(&locked.path, &line);
    }
    validate_backup(&backup)?;
    // Backup publication happens first; a failed candidate leaves old history
    // in place. Never move/delete the active file before the new file is synced.
    atomic_file::copy_private(&locked.path, &backup)?;
    atomic_file::write_stream(&locked.path, |out| {
        let skip = rows.saturating_sub(limit.saturating_sub(1));
        let mut reader = BufReader::new(File::open(&locked.path)?);
        let mut row = Vec::new();
        let mut index = 0;
        while read_row(&mut reader, &mut row)? {
            if index >= skip {
                out.write_all(&row)?;
                if !row.ends_with(b"\n") {
                    out.write_all(b"\n")?;
                }
            }
            index += 1;
        }
        out.write_all(&line)
    })
}

fn separate_final_row(path: &Path, line: &mut Vec<u8>) -> io::Result<()> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut final_byte = [0];
        file.read_exact(&mut final_byte)?;
        if final_byte[0] != b'\n' {
            line.insert(0, b'\n');
        }
    }
    Ok(())
}

/// Canonicalize existing ancestors without creating a disabled metrics writer's
/// directories. Missing normal components are safe to retain; dangling aliases,
/// parent traversal through missing directories, and access errors fail closed.
fn metrics_identity(path: &Path) -> io::Result<std::path::PathBuf> {
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        let candidate = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &ancestor
        };
        match fs::canonicalize(candidate) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::symlink_metadata(candidate) {
                    Ok(_) => {
                        return Err(io::Error::other("metrics destination is a dangling alias"))
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                match ancestor.components().next_back() {
                    Some(std::path::Component::Normal(name)) => missing.push(name.to_os_string()),
                    _ => return Err(error),
                }
                ancestor.pop();
            }
            Err(error) => return Err(error),
        }
    }
}

fn inspect(path: &Path) -> io::Result<(usize, bool)> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, true)),
        Err(error) => return Err(error),
    };
    let mut reader = BufReader::new(file);
    let mut row = Vec::new();
    let mut count = 0usize;
    let mut final_newline = true;
    while read_row(&mut reader, &mut row)? {
        let value: Value = serde_json::from_slice(&row).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "history contains malformed/partial rows; no pruning performed",
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "history contains non-object rows; no pruning performed",
            )
        })?;
        // Never silently prune an unrecognized/shared metrics format.
        if crate::telemetry::history_event(&value).as_object() != Some(object) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "history contains non-history fields; no pruning performed",
            ));
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| io::Error::other("history row count overflow"))?;
        final_newline = row.ends_with(b"\n");
    }
    Ok((count, final_newline))
}

fn read_row(reader: &mut impl BufRead, row: &mut Vec<u8>) -> io::Result<bool> {
    row.clear();
    let length = reader
        .take((MAX_LINE_BYTES + 1) as u64)
        .read_until(b'\n', row)?;
    if length > MAX_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "history row exceeds retention safety limit; original preserved",
        ));
    }
    Ok(length > 0)
}

fn validate_backup(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::other(
            "history recovery backup is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "history_retention_tests.rs"]
mod tests;
