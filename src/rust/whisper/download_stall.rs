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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
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

/// Upper bound on how long a cancellation request waits for the consumer loop.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Shared cancellation state for a single model download.
#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    active_workers: AtomicUsize,
    model: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadCancellation(Arc<CancellationState>);

/// Keeps a transfer worker registered until its blocking operation returns.
pub(crate) struct DownloadWorker(DownloadCancellation);

impl Drop for DownloadWorker {
    fn drop(&mut self) {
        self.0 .0.active_workers.fetch_sub(1, Ordering::SeqCst);
        if crate::diag::debug_enabled() {
            crate::diag::log!(
                "[models] debug: download worker released for {} (active_workers={})",
                self.0 .0.model,
                self.0.active_workers()
            );
        }
    }
}

impl DownloadCancellation {
    pub(crate) fn for_model(model: &'static str) -> Self {
        Self(Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            active_workers: AtomicUsize::new(0),
            model,
        }))
    }

    pub(crate) fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Register a blocking worker. Callers retain the returned guard in the
    /// worker so a cancelled transfer cannot be retried until its socket is
    /// actually released.
    pub(crate) fn worker(&self) -> DownloadWorker {
        self.0.active_workers.fetch_add(1, Ordering::SeqCst);
        DownloadWorker(self.clone())
    }

    pub(crate) fn has_active_workers(&self) -> bool {
        self.active_workers() != 0
    }

    pub(crate) fn active_workers(&self) -> usize {
        self.0.active_workers.load(Ordering::SeqCst)
    }
}

impl Default for DownloadCancellation {
    fn default() -> Self {
        Self::for_model("unspecified")
    }
}

/// File locations and integrity data for one download attempt.
pub(crate) struct DownloadTarget<'a> {
    partial: &'a Path,
    target: &'a Path,
    expected_sha256: &'a str,
}

impl<'a> DownloadTarget<'a> {
    pub(crate) fn new(partial: &'a Path, target: &'a Path, expected_sha256: &'a str) -> Self {
        Self {
            partial,
            target,
            expected_sha256,
        }
    }
}

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

/// The error [`ChunkReader`] synthesizes when its idle window expires.
///
/// Exists as a concrete type purely so the caller can tell it apart from other
/// timeouts. `ErrorKind::TimedOut` is NOT a sufficient discriminator: ureq's
/// body reader returns that same kind when the *global* transfer budget expires
/// on a healthy but slow download, and reporting that as a stall would point
/// the user at the wrong env var entirely.
///
/// The message is the tail of the caller's — which prefixes "download stalled:"
/// — so it deliberately avoids repeating the word "stalled".
#[derive(Debug)]
struct StalledTransfer {
    idle: Duration,
}

impl std::fmt::Display for StalledTransfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no data received for {}s (the server accepted the connection then \
             went silent); retry, or raise {IDLE_TIMEOUT_ENV}",
            self.idle.as_secs()
        )
    }
}

impl std::error::Error for StalledTransfer {}

#[derive(Debug)]
struct CancelledTransfer;

impl std::fmt::Display for CancelledTransfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("download cancelled")
    }
}

impl std::error::Error for CancelledTransfer {}

/// True only for a stall this module synthesized — see [`StalledTransfer`].
///
/// Callers must use this rather than checking `ErrorKind::TimedOut`, which a
/// transport-level timeout also carries.
pub(crate) fn is_stall(err: &std::io::Error) -> bool {
    err.get_ref()
        .is_some_and(|inner| inner.is::<StalledTransfer>())
}

/// True only when the Settings cancel action stopped this download.
pub(crate) fn is_cancelled(err: &std::io::Error) -> bool {
    err.get_ref()
        .is_some_and(|inner| inner.is::<CancelledTransfer>())
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
    cancellation: DownloadCancellation,
    total: Option<u64>,
    target: DownloadTarget<'_>,
    cb: &dyn DownloadProgress,
) -> Result<()> {
    let mut chunks = ChunkReader::spawn(reader, idle, cancellation);
    stream_download_to(
        &mut chunks,
        total,
        target.partial,
        target.target,
        target.expected_sha256,
        cb,
    )
}

/// `Read` adapter over a background reader thread, enforcing an idle deadline.
///
/// Implementing `Read` — rather than reworking the download loop — keeps the
/// hashing / writing / verification path in `stream_download_to` completely
/// unchanged, so there is no second copy of the cleanup logic to keep in sync.
struct ChunkReader {
    rx: Receiver<std::io::Result<Vec<u8>>>,
    idle: Duration,
    cancellation: DownloadCancellation,
    /// Chunk currently being handed out, and how much of it the caller took.
    /// Needed because the caller's buffer may be smaller than one chunk.
    chunk: Vec<u8>,
    pos: usize,
    /// Latched once the stream ends or errors, so a caller that keeps reading
    /// past EOF does not wait out another whole idle window.
    finished: bool,
}

impl ChunkReader {
    fn spawn<R: Read + Send + 'static>(
        mut reader: R,
        idle: Duration,
        cancellation: DownloadCancellation,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<Vec<u8>>>(READ_AHEAD_CHUNKS);
        let worker = cancellation.worker();
        std::thread::spawn(move || {
            let _worker = worker;
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
            cancellation,
            chunk: Vec::new(),
            pos: 0,
            finished: false,
        }
    }

    /// Pull the next chunk into `self.chunk`, or report end-of-stream.
    fn fill(&mut self) -> std::io::Result<bool> {
        let started = std::time::Instant::now();
        loop {
            if self.cancellation.is_cancelled() {
                self.finished = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    CancelledTransfer,
                ));
            }
            let elapsed = started.elapsed();
            if elapsed >= self.idle {
                self.finished = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    StalledTransfer { idle: self.idle },
                ));
            }
            let wait = (self.idle - elapsed).min(CANCEL_POLL_INTERVAL);
            match self.rx.recv_timeout(wait) {
                Ok(Ok(chunk)) => {
                    self.chunk = chunk;
                    self.pos = 0;
                    return Ok(true);
                }
                Ok(Err(err)) => {
                    self.finished = true;
                    return Err(err);
                }
                Err(RecvTimeoutError::Timeout) => continue,
                // Sender dropped without an error: clean end of stream.
                Err(RecvTimeoutError::Disconnected) => {
                    self.finished = true;
                    return Ok(false);
                }
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
