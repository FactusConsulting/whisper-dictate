//! Streaming JSONL scans with bounded rows and bounded recent-result storage.

use serde_json::Value;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

pub(crate) const MAX_ROW_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_TAIL_ROWS: usize = 1000;
const MAX_TAIL_BYTES: usize = 8 * 1024 * 1024;

/// Malformed, oversized, and invalid-UTF-8 rows are skipped independently.
/// The callback is called in file order; even one hostile line cannot grow
/// the buffer beyond MAX_ROW_BYTES.
pub(crate) fn scan(path: &Path, mut visit: impl FnMut(Value, usize)) -> io::Result<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if !oversized {
                parse_line(&line, &mut visit);
            }
            return Ok(());
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(buffer.len(), |position| position + 1);
        if !oversized {
            if line.len() + count > MAX_ROW_BYTES {
                line.clear();
                oversized = true;
            } else {
                line.extend_from_slice(&buffer[..count]);
            }
        }
        reader.consume(count);
        if newline.is_some() {
            if !oversized {
                parse_line(&line, &mut visit);
            }
            line.clear();
            oversized = false;
        }
    }
}

fn parse_line(line: &[u8], visit: &mut impl FnMut(Value, usize)) {
    if let Ok(text) = std::str::from_utf8(line) {
        if let Ok(value) = serde_json::from_str(text.trim()) {
            visit(value, line.len());
        }
    }
}

pub(crate) struct Tail {
    rows: VecDeque<(Value, usize)>,
    limit: usize,
    bytes: usize,
}

impl Tail {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            rows: VecDeque::new(),
            limit: limit.clamp(1, MAX_TAIL_ROWS),
            bytes: 0,
        }
    }

    pub(crate) fn push(&mut self, row: Value, bytes: usize) {
        self.bytes += bytes;
        self.rows.push_back((row, bytes));
        while self.rows.len() > self.limit || self.bytes > MAX_TAIL_BYTES {
            let (_, bytes) = self.rows.pop_front().expect("nonempty bounded tail");
            self.bytes -= bytes;
        }
    }

    pub(crate) fn into_rows(self) -> Vec<Value> {
        self.rows.into_iter().map(|(row, _)| row).collect()
    }
}

#[cfg(test)]
#[path = "jsonl_tests.rs"]
mod tests;
