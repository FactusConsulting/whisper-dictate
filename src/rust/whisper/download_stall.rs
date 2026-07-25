//! Stall detection for model downloads (#573).
//!
//! A model host that accepts the connection and then goes silent mid-body used
//! to block the download until the entire global budget elapsed — six hours,
//! with the CLI hung and every Settings download button disabled throughout.
//! `timeout_connect` does not help: it only covers *establishing* the
//! connection, not a stall after the first byte.
//!
//! ureq 3.x cannot express an idle timeout. Every knob it has —
//! `timeout_global` / `per_call` / `resolve` / `connect` / `send_request` /
//! `await_100` / `send_body` / `recv_response` / `recv_body` — is a per-stage
//! TOTAL. `timeout_recv_body` bounds the whole body transfer, so any value
//! short enough to catch a stall would also abort a legitimately slow multi-GB
//! download. The two requirements are in direct tension under ureq's model.
//!
//! So we detect the stall where we already own the loop: reads run on a worker
//! thread feeding a bounded channel, and the download loop consumes it with
//! `recv_timeout`. That distinction — *no bytes for N seconds* vs. *not
//! finished within N seconds* — is exactly what a throughput-agnostic stall
//! detector needs, and it costs one thread per download.
//!
//! Split into its own module rather than added to `model_manager` because that
//! file is already at the repo's ~500-line ceiling.

use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use anyhow::Result;

use super::model_manager::{download_timeout, stream_download_to, DownloadProgress};

/// Fail a download when no bytes at all arrive for this long (default 2 min).
///
/// Deliberately forgiving. The bar is *any* progress within the window, not a
/// throughput floor: even at 0.5 Mbit/s a 64 KB chunk completes roughly every
/// second, so a slow-but-alive link resets the clock ~120 times over before it
/// could trip. Only a genuinely silent socket does.
pub(crate) const IDLE_TIMEOUT_SECS: u64 = 120;

/// Env override for [`IDLE_TIMEOUT_SECS`]; `0` disables stall detection.
pub(crate) const IDLE_TIMEOUT_ENV: &str = "VOICEPI_MODEL_DOWNLOAD_IDLE_TIMEOUT_SECS";

/// How many chunks the reader thread may buffer ahead of the writer.
///
/// Bounded on purpose: an unbounded channel would let a fast server buffer the
/// entire multi-GB model in RAM while the hash/write side falls behind. Four
/// 64 KB chunks keeps the socket busy across a write hiccup and caps the
/// read-ahead at 256 KB.
const READ_AHEAD_CHUNKS: usize = 4;

/// Chunk size for the reader thread. Matches the buffer `stream_download_to`
/// uses, so neither side re-splits the other's chunks.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// Idle window for stall detection, with `0` meaning "disabled".
///
/// Disabling does not mean "wait forever": it falls back to the global transfer
/// budget, so the wait stays bounded by something the user already configured
/// rather than by an unbounded `recv`. An unparseable value falls back to the
/// default, matching how `download_timeout` treats garbage.
pub(crate) fn idle_timeout() -> Duration {
    let secs = std::env::var(IDLE_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(IDLE_TIMEOUT_SECS);
    if secs == 0 {
        return download_timeout();
    }
    Duration::from_secs(secs)
}

/// [`stream_download_to`] with a stall detector wrapped around `reader`.
///
/// A socket that goes silent fails after `idle` rather than after the whole
/// global budget, and the partial file is removed on the way out (the existing
/// read-error cleanup in `stream_download_to` handles it, since a stall
/// surfaces as an ordinary `io::Error`). A slow-but-progressing transfer is
/// untouched: every chunk, however small, resets the window.
///
/// The worker thread is abandoned, not joined, when the window trips — it is
/// parked in a blocking `read` on a dead socket and there is no portable way to
/// interrupt that. It exits on its own when the OS TCP timeout fires, or
/// immediately if it was blocked on `send`, since dropping the receiver wakes
/// it. One parked thread per stalled download is the price of failing fast; the
/// alternative is the *caller* blocking for that same duration.
pub(crate) fn stream_download_with_idle_timeout<R: Read + Send + 'static>(
    reader: R,
    idle: Duration,
    total: Option<u64>,
    partial: &Path,
    target: &Path,
    expected_sha256: &str,
    cb: &dyn DownloadProgress,
) -> Result<()> {
    let mut chunks = ChunkReader::spawn(reader, idle);
    stream_download_to(&mut chunks, total, partial, target, expected_sha256, cb)
}

/// `Read` adapter over a background reader thread, enforcing an idle deadline.
///
/// Implementing `Read` — rather than reworking the download loop — keeps the
/// hashing / writing / verification path in `stream_download_to` completely
/// unchanged, so there is no second copy of the cleanup logic to keep in sync.
struct ChunkReader {
    rx: Receiver<std::io::Result<Vec<u8>>>,
    idle: Duration,
    /// Chunk currently being handed out, and how much of it the caller took.
    /// Needed because the caller's buffer may be smaller than one chunk.
    chunk: Vec<u8>,
    pos: usize,
    /// Latched once the stream ends or errors, so a caller that keeps reading
    /// past EOF does not wait out another whole idle window.
    finished: bool,
}

impl ChunkReader {
    fn spawn<R: Read + Send + 'static>(mut reader: R, idle: Duration) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<Vec<u8>>>(READ_AHEAD_CHUNKS);
        std::thread::spawn(move || {
            let mut buf = vec![0u8; READ_CHUNK_BYTES];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        // `send` fails once the receiver is gone (stall, or the
                        // caller bailing out) — the worker's exit signal.
                        if tx.send(Ok(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => {
                        let _ = tx.send(Err(err));
                        break;
                    }
                }
            }
        });
        Self {
            rx,
            idle,
            chunk: Vec::new(),
            pos: 0,
            finished: false,
        }
    }

    /// Pull the next chunk into `self.chunk`, or report end-of-stream.
    fn fill(&mut self) -> std::io::Result<bool> {
        match self.rx.recv_timeout(self.idle) {
            Ok(Ok(chunk)) => {
                self.chunk = chunk;
                self.pos = 0;
                Ok(true)
            }
            Ok(Err(err)) => {
                self.finished = true;
                Err(err)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.finished = true;
                // Wording is the tail of the caller's message, which prefixes
                // "download stalled: " — see `stream_download_to`. Keeping the
                // word "stalled" out of here avoids "stalled: ... stalled".
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "no data received for {}s (server accepted the \
                         connection then went silent); retry, or raise {IDLE_TIMEOUT_ENV}",
                        self.idle.as_secs()
                    ),
                ))
            }
            // Sender dropped without an error: clean end of stream.
            Err(RecvTimeoutError::Disconnected) => {
                self.finished = true;
                Ok(false)
            }
        }
    }
}

impl Read for ChunkReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        // Loop rather than `if`: a zero-length chunk must not reach the caller
        // as a false EOF.
        while self.pos >= self.chunk.len() {
            if self.finished || !self.fill()? {
                return Ok(0);
            }
        }
        let n = out.len().min(self.chunk.len() - self.pos);
        out[..n].copy_from_slice(&self.chunk[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
#[path = "download_stall_tests.rs"]
mod tests;
