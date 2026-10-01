//! Explicit opt-in pruning; bounded row buffers, one recovery generation.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

use serde_json::Value;

use crate::{atomic_file, jsonl_file};

const MAX_LINE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ENTRIES: usize = 100_000;

pub(crate) fn parse_limit(value: &str) -> usize {
    value.trim().parse::<usize>().ok().filter(|limit| *limit <= MAX_ENTRIES).unwrap_or(0)
}

pub(crate) fn append(path: &Path, event: &Value, limit: usize, metrics_path: Option<&Path>) -> io::Result<()> {
    if limit > MAX_ENTRIES {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "history retention limit is out of range"));
    }
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    if limit > 0 && line.len() > MAX_LINE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "history row exceeds retention safety limit"));
    }
    let locked = jsonl_file::acquire(path)?;
    if limit == 0 {
        return jsonl_file::append_locked(&locked.path, &line);
    }
    let backup = jsonl_file::sibling(&locked.path, ".retention-backup")?;
    if let Some(metrics) = metrics_path {
        // A missing metrics parent need not exist yet; exact normalized paths
        // catch configured aliases where possible, and unreadable identities
        // fail closed rather than guessing that pruning would be safe.
        if jsonl_file::identity(metrics)? == locked.path || jsonl_file::identity(metrics)? == backup {
            return Err(io::Error::other("history retention requires a separate file from metrics"));
        }
    }
    let (rows, final_newline) = inspect(&locked.path)?;
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
                if !row.ends_with(b"\n") { out.write_all(b"\n")?; }
            }
            index += 1;
        }
        out.write_all(&line)
    })
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
        let value: Value = serde_json::from_slice(&row).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "history contains malformed/partial rows; no pruning performed"))?;
        let object = value.as_object().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "history contains non-object rows; no pruning performed"))?;
        // Never silently prune an unrecognized/shared metrics format.
        if crate::telemetry::history_event(&value).as_object() != Some(object) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "history contains non-history fields; no pruning performed"));
        }
        count = count.checked_add(1).ok_or_else(|| io::Error::other("history row count overflow"))?;
        final_newline = row.ends_with(b"\n");
    }
    Ok((count, final_newline))
}

fn read_row(reader: &mut impl BufRead, row: &mut Vec<u8>) -> io::Result<bool> {
    row.clear();
    let length = reader.take((MAX_LINE_BYTES + 1) as u64).read_until(b'\n', row)?;
    if length > MAX_LINE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "history row exceeds retention safety limit; original preserved"));
    }
    Ok(length > 0)
}

fn validate_backup(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::other("history recovery backup is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "history_retention_tests.rs"]
mod tests;
