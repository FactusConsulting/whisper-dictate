//! Unit tests for [`super`] (`whisper::download_stall`).
//!
//! The whole point of the module is a distinction that only shows up in the
//! time domain — *no bytes for N seconds* must fail, *not finished within N
//! seconds* must not. The idle-window decision is driven through a
//! [`VirtualFeed`], whose clock only moves while the reader waits, so exact
//! gaps on either side of the window cost no real time and cannot be skewed
//! by a starved thread. One real-thread test keeps the production channel and
//! `Instant` wiring honest, with a one-sided bound far above its window.
//!
//! Env-var parsing for `idle_timeout()` is covered in `model_manager_tests.rs`,
//! which already owns the process-wide env lock and its guard.

use super::*;
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::sync::mpsc;

/// Reader that serves one chunk and then goes silent forever — a server that
/// accepted the connection and stopped sending mid-body.
///
/// It blocks on a channel the test owns rather than sleeping, so dropping the
/// sender at the end of the test releases the worker thread instead of leaving
/// it parked for the rest of the run.
struct SilentAfterFirstChunk {
    prefix: Vec<u8>,
    served: bool,
    gate: mpsc::Receiver<()>,
}

impl io::Read for SilentAfterFirstChunk {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.served {
            let n = buf.len().min(self.prefix.len());
            buf[..n].copy_from_slice(&self.prefix[..n]);
            self.served = true;
            return Ok(n);
        }
        // Blocks until the test drops its sender.
        let _ = self.gate.recv();
        Ok(0)
    }
}

/// Virtual silence after which a [`VirtualFeed`] declares the reader hung. Far
/// above every window these tests use, so only a reader that never aborts can
/// reach it.
const VIRTUAL_HORIZON: Duration = Duration::from_secs(3600);

/// Chunk feed on a virtual clock: each scripted item arrives a fixed gap after
/// the previous arrival, and time only advances while the reader waits in
/// `recv_timeout`. Once the script runs out the feed either reports a clean
/// end of stream or stays silent forever.
struct VirtualFeed {
    origin: Instant,
    now: Cell<Duration>,
    last_arrival: Cell<Duration>,
    script: RefCell<VecDeque<(Duration, io::Result<Vec<u8>>)>>,
    eof_after_script: bool,
    cancel_at: Option<(Duration, DownloadCancellation)>,
    recv_calls: Cell<usize>,
}

impl VirtualFeed {
    fn new(script: Vec<(Duration, io::Result<Vec<u8>>)>, eof_after_script: bool) -> Self {
        Self {
            origin: Instant::now(),
            now: Cell::new(Duration::ZERO),
            last_arrival: Cell::new(Duration::ZERO),
            script: RefCell::new(script.into()),
            eof_after_script,
            cancel_at: None,
            recv_calls: Cell::new(0),
        }
    }

    /// Cancel `cancellation` once virtual time reaches `at`, the way the
    /// Settings button does from another thread while the reader waits.
    fn cancelling_at(mut self, at: Duration, cancellation: DownloadCancellation) -> Self {
        self.cancel_at = Some((at, cancellation));
        self
    }

    fn elapsed(&self) -> Duration {
        self.now.get()
    }
}

impl ChunkFeed for &VirtualFeed {
    fn now(&self) -> Instant {
        self.origin + self.now.get()
    }

    fn recv_timeout(
        &self,
        wait: Duration,
    ) -> std::result::Result<io::Result<Vec<u8>>, RecvTimeoutError> {
        self.recv_calls.set(self.recv_calls.get() + 1);
        let now = self.now.get();
        let mut script = self.script.borrow_mut();
        let due = match script.front() {
            Some((gap, _)) => Some(self.last_arrival.get() + *gap),
            None if self.eof_after_script => return Err(RecvTimeoutError::Disconnected),
            None => None,
        };
        if let Some(due) = due.filter(|due| *due <= now + wait) {
            let at = due.max(now);
            self.now.set(at);
            self.last_arrival.set(at);
            let (_, item) = script.pop_front().expect("front was just inspected");
            return Ok(item);
        }
        let after = now + wait;
        assert!(
            after < VIRTUAL_HORIZON,
            "the reader waited out {after:?} of virtual silence without aborting",
        );
        self.now.set(after);
        if let Some((at, cancellation)) = &self.cancel_at {
            if after >= *at {
                cancellation.cancel();
            }
        }
        Err(RecvTimeoutError::Timeout)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

#[test]
fn stalled_transfer_fails_within_the_idle_window_and_cleans_the_partial() {
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("model.bin.partial");
    let target = tmp.path().join("model.bin");
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let reader = SilentAfterFirstChunk {
        prefix: b"the first few bytes arrive just fine".to_vec(),
        served: false,
        gate: gate_rx,
    };
    let idle = Duration::from_millis(250);

    let started = std::time::Instant::now();
    let err = stream_download_with_idle_timeout(
        reader,
        idle,
        DownloadCancellation::default(),
        None,
        DownloadTarget::new(&partial, &target, &"00".repeat(32)),
        &(),
    )
    .expect_err("a silent server must fail the download");
    let elapsed = started.elapsed();

    let msg = err.to_string();
    assert!(
        msg.contains("download stalled"),
        "a stall must be reported distinctly from a generic read failure: {msg}",
    );
    assert!(
        msg.contains("no data received"),
        "the message must say what actually happened: {msg}",
    );
    assert!(
        msg.contains(IDLE_TIMEOUT_ENV),
        "the message must name the escape hatch so a user on a pathological \
         link can raise it instead of guessing: {msg}",
    );
    assert_eq!(
        msg.matches("stall").count(),
        1,
        "the wrapper prefix and the inner message must not both say 'stalled': {msg}",
    );
    // Bounded by the idle window, NOT by the 6-hour global transfer budget.
    // The bound is far above the window because a loaded runner can pause
    // this thread for seconds; exact window arithmetic is covered on the
    // virtual clock.
    assert!(
        elapsed < Duration::from_secs(60),
        "stall must be detected within the idle window, took {elapsed:?}",
    );
    assert!(!target.exists(), "no target may be installed from a stall");
    assert!(
        !partial.exists(),
        "the partial must be removed so the next attempt starts clean",
    );
    drop(gate_tx);
}

#[test]
fn cancellation_stops_a_silent_transfer_without_waiting_for_the_idle_window() {
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("model.bin.partial");
    let target = tmp.path().join("model.bin");
    let cancellation = DownloadCancellation::default();
    let cancel_at = Duration::from_millis(30);
    let feed = VirtualFeed::new(
        vec![(
            Duration::ZERO,
            Ok(b"the first few bytes arrive just fine".to_vec()),
        )],
        false,
    )
    .cancelling_at(cancel_at, cancellation.clone());
    let mut chunks = ChunkReader::new(&feed, Duration::from_secs(30), cancellation);

    let err = stream_download_to(&mut chunks, None, &partial, &target, &"00".repeat(32), &())
        .expect_err("a cancellation request must stop the download");

    assert!(err.to_string().contains("download cancelled"), "{err}");
    assert!(
        feed.elapsed() <= cancel_at + CANCEL_POLL_INTERVAL,
        "cancellation must be noticed within one poll interval, not after the \
         idle window: noticed at {:?}",
        feed.elapsed(),
    );
    assert!(!target.exists());
    assert!(!partial.exists());
}

#[test]
fn slow_but_progressing_transfer_is_never_aborted_by_the_idle_window() {
    // A per-transfer deadline short enough to catch a stall would kill this
    // download: it spans dozens of idle windows end to end, but no single gap
    // between chunks reaches the window.
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("slow.bin.partial");
    let target = tmp.path().join("slow.bin");
    let idle = Duration::from_secs(3);
    let gap = idle - Duration::from_millis(1);
    let chunks: Vec<Vec<u8>> = (0..80u8).map(|i| vec![i; 1024]).collect();
    let payload: Vec<u8> = chunks.iter().flatten().copied().collect();
    let expected = sha256_hex(&payload);
    let feed = VirtualFeed::new(
        chunks.into_iter().map(|chunk| (gap, Ok(chunk))).collect(),
        true,
    );
    let mut reader = ChunkReader::new(&feed, idle, DownloadCancellation::default());

    stream_download_to(&mut reader, None, &partial, &target, &expected, &())
        .expect("a slow but progressing transfer must complete");

    assert_eq!(
        feed.elapsed(),
        gap * 80,
        "the transfer must run its full virtual length",
    );
    assert!(
        feed.elapsed() > idle * 50,
        "the scenario must span many idle windows",
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        payload,
        "every chunk must survive the hand-off in order",
    );
    assert!(!partial.exists(), "partial must be renamed into place");
}

#[test]
fn one_gap_longer_than_the_idle_window_aborts_measured_from_the_last_chunk() {
    use std::io::Read as _;

    let idle = Duration::from_secs(3);
    let second = Duration::from_secs(1);
    let feed = VirtualFeed::new(
        vec![
            (second, Ok(b"first".to_vec())),
            (second, Ok(b"second".to_vec())),
            (idle + second, Ok(b"too late".to_vec())),
            (second, Ok(b"never read".to_vec())),
        ],
        true,
    );
    let mut reader = ChunkReader::new(&feed, idle, DownloadCancellation::default());

    let mut received = Vec::new();
    let mut buf = [0u8; 64];
    let err = loop {
        match reader.read(&mut buf) {
            Ok(0) => panic!("a gap longer than the idle window must abort the transfer"),
            Ok(n) => received.extend_from_slice(&buf[..n]),
            Err(err) => break err,
        }
    };

    assert!(is_stall(&err), "the abort must be our stall marker: {err}");
    assert_eq!(
        received, b"firstsecond",
        "chunks before the long gap must be delivered"
    );
    assert_eq!(
        feed.elapsed(),
        second * 2 + idle,
        "the window must run from the last chunk, not from the transfer start",
    );
}

#[test]
fn reader_errors_still_surface_as_read_failures_not_stalls() {
    // The stall path must not swallow or relabel a genuine transport error:
    // "download stalled" and "download read failed" point at different
    // remedies, so a broken pipe has to keep its own wording.
    struct FailingReader;
    impl io::Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("synthetic transport failure"))
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("model.bin.partial");
    let target = tmp.path().join("model.bin");

    let err = stream_download_with_idle_timeout(
        FailingReader,
        Duration::from_secs(30),
        DownloadCancellation::default(),
        None,
        DownloadTarget::new(&partial, &target, &"00".repeat(32)),
        &(),
    )
    .expect_err("a reader error must propagate through the channel");

    let msg = err.to_string();
    assert!(
        msg.contains("download read failed"),
        "transport errors keep the read-failure wording: {msg}",
    );
    assert!(
        !msg.contains("stalled"),
        "a hard error must not be reported as a stall: {msg}",
    );
    assert!(!partial.exists(), "partial must be cleaned up");
}

#[test]
fn a_transport_timeout_is_not_misreported_as_a_stall() {
    // off `ErrorKind::TimedOut`, but ureq's body reader returns exactly that
    // when the GLOBAL transfer budget expires on a healthy, continuously
    // progressing slow download. That user's problem is
    // VOICEPI_MODEL_DOWNLOAD_TIMEOUT_SECS being too low — telling them to raise
    // the idle timeout would send them chasing the wrong knob entirely.
    struct GlobalBudgetExpired;
    impl io::Read for GlobalBudgetExpired {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let partial = tmp.path().join("model.bin.partial");
    let target = tmp.path().join("model.bin");

    let err = stream_download_with_idle_timeout(
        GlobalBudgetExpired,
        // Idle window far larger than the test needs: the error must come from
        // the reader, not from our own deadline.
        Duration::from_secs(600),
        DownloadCancellation::default(),
        None,
        DownloadTarget::new(&partial, &target, &"00".repeat(32)),
        &(),
    )
    .expect_err("a transport timeout must still fail the download");

    let msg = err.to_string();
    assert!(
        !msg.contains("stalled"),
        "a transport timeout is not a stall: {msg}",
    );
    assert!(
        !msg.contains(IDLE_TIMEOUT_ENV),
        "must not point the user at the idle timeout for a global-budget \
         expiry: {msg}",
    );
    assert!(
        msg.contains("VOICEPI_MODEL_DOWNLOAD_TIMEOUT_SECS"),
        "must name the var that actually governs this failure: {msg}",
    );
    assert!(msg.is_ascii(), "see the ASCII guard below: {msg}");
    assert!(!partial.exists(), "partial must be cleaned up either way");
}

#[test]
fn download_failure_messages_are_ascii() {
    // AGENTS.md: stdout/stderr must survive cmd.exe, PowerShell on a legacy
    // code page, and hidden launchers. These strings reach stderr through
    // `models_cli::run_download`, so a stray em dash or curly quote is a real
    let stalled = StalledTransfer {
        idle: Duration::from_secs(120),
    }
    .to_string();
    assert!(stalled.is_ascii(), "stall message must be ASCII: {stalled}");
    // The transport-timeout and generic wordings are asserted at their own
    // call sites, where the full formatted string is available.
}

#[test]
fn is_stall_only_matches_our_own_marker() {
    // Unit-level guard on the discriminator itself, so the distinction survives
    // even if the wording in `stream_download_to` is reworded later.
    let ours = io::Error::new(
        io::ErrorKind::TimedOut,
        StalledTransfer {
            idle: Duration::from_secs(30),
        },
    );
    assert!(is_stall(&ours), "our own marker must be recognised");
    assert!(
        !is_stall(&io::Error::new(io::ErrorKind::TimedOut, "timed out")),
        "a same-kind transport timeout must NOT be treated as a stall",
    );
    assert!(!is_stall(&io::Error::other("broken pipe")));
}

#[test]
fn chunk_reader_serves_a_chunk_across_several_short_reads() {
    // `stream_download_to` reads into a 64 KiB buffer, the same size the worker
    // produces, so the split-chunk path would otherwise never run in practice.
    // Drive it directly: a caller whose buffer is smaller than one chunk must
    // still see every byte, in order, exactly once.
    use std::io::Read as _;

    let payload: Vec<u8> = (0..=255u8).collect();
    let mut reader = ChunkReader::spawn(
        io::Cursor::new(payload.clone()),
        Duration::from_secs(30),
        DownloadCancellation::default(),
    );
    let mut out = Vec::new();
    let mut buf = [0u8; 7];
    loop {
        let n = reader.read(&mut buf).expect("no error on a healthy stream");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    assert_eq!(out, payload, "short reads must reassemble the full stream");
}

#[test]
fn chunk_reader_reports_eof_repeatedly_without_waiting_out_the_window() {
    // A caller that reads once more after EOF must get an immediate `Ok(0)`
    // from the latched `finished` flag, not another wait on the feed.
    use std::io::Read as _;

    let feed = VirtualFeed::new(Vec::new(), true);
    let mut reader = ChunkReader::new(
        &feed,
        Duration::from_secs(600),
        DownloadCancellation::default(),
    );
    let mut buf = [0u8; 8];
    assert_eq!(reader.read(&mut buf).unwrap(), 0, "empty stream is EOF");
    assert_eq!(reader.read(&mut buf).unwrap(), 0, "EOF is sticky");
    assert_eq!(
        feed.recv_calls.get(),
        1,
        "reads past EOF must not wait on the feed again",
    );
}

#[test]
fn zero_length_caller_buffer_is_not_mistaken_for_eof() {
    use std::io::Read as _;

    let mut reader = ChunkReader::spawn(
        io::Cursor::new(b"abc".to_vec()),
        Duration::from_secs(30),
        DownloadCancellation::default(),
    );
    assert_eq!(
        reader.read(&mut []).unwrap(),
        0,
        "a zero-length read is trivially zero bytes",
    );
    let mut buf = [0u8; 8];
    assert_eq!(
        reader.read(&mut buf).unwrap(),
        3,
        "the stream must still be readable afterwards",
    );
}
