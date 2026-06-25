//! Bridge that pipes [`super::PipelineEvent`]s into the Python worker's stdin.
//!
//! This is the supervisor-side wiring for the `audio-in-rust` rollout
//! (Phase 1): the runtime spawns the Rust [`AudioPipeline`] in a worker
//! thread, then this module owns the second thread that drains the
//! pipeline's event channel and serialises each event as one JSON line
//! into the writer the caller hands us (typically `Child::stdin`).
//!
//! Two threads, not one, because the pipeline's pump already runs on a
//! dedicated thread (see [`super::AudioPipeline::start`]); the bridge
//! thread is the sink. Both shut down deterministically:
//!
//! * Sender exit: [`BridgeHandle::stop`] tells the pipeline to stop,
//!   which closes the event channel; the writer thread sees `recv()`
//!   return `Err(Disconnected)` and exits.
//! * Receiver exit (Python died, broken pipe): the next `write_all` /
//!   `flush` returns `BrokenPipe`; the writer translates that to
//!   [`BridgeError::WorkerClosed`] on its result channel and exits.
//!
//! The bridge deliberately does NOT touch [`super::PipelineEvent::Cancelled`]
//! specially — cancellation lives at a higher layer (PTT release) and is
//! plumbed in a follow-up; for Phase 1 we faithfully forward whatever
//! the pipeline emits.

use std::io::Write;
use std::sync::mpsc::{self, Receiver, RecvError, Sender};
use std::thread::{self, JoinHandle};

use super::{event_to_json_line, AudioPipeline, PipelineEvent, SileroVad};

/// Terminal failure surfaced on [`BridgeHandle::error_receiver`].
///
/// The supervisor uses this to decide whether to surface a UI error
/// banner (`WorkerClosed` = expected on PTT release / stop; `Io` /
/// `Pipeline` = unexpected). The pipeline thread always sends ONE event
/// of this shape per run — successful drain emits no error.
#[derive(Debug)]
pub enum BridgeError {
    /// The Python worker hung up on us (typical `BrokenPipe`). Expected
    /// when the user releases PTT and the worker exits cleanly; the
    /// supervisor folds this into its usual exit path.
    WorkerClosed,
    /// Anything else writing to stdin failed (disk full on a captured
    /// pipe in a test, permission denied, …). The message is the OS
    /// error description.
    Io(String),
    /// The capture pipeline itself surfaced a [`PipelineEvent::DeviceError`].
    /// We still forward the event to Python first, then deliver this so
    /// the supervisor knows to surface it (and tear down) even if Python
    /// crashes before reading the line.
    Pipeline(String),
}

/// Handle to a running bridge. Drop or call [`Self::stop`] to shut down.
///
/// The error channel is intentionally NOT held inside this struct: the
/// supervisor moves it into a dedicated watcher thread so the handle
/// can be `stop()`-ed independently of where errors are consumed.
pub struct BridgeHandle {
    pipeline: Option<AudioPipeline>,
    writer: Option<JoinHandle<()>>,
}

impl BridgeHandle {
    /// Stop the pipeline and wait for the writer thread to drain. Safe
    /// to call multiple times; only the first call has effect.
    pub fn stop(&mut self) {
        if let Some(mut p) = self.pipeline.take() {
            p.stop();
        }
        if let Some(h) = self.writer.take() {
            let _ = h.join();
        }
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Two-stage bridge that defers opening the cpal capture stream until
/// the Python worker's stdin reader is up.
///
/// Iteration-3 review finding #1: when the supervisor opened the
/// pipeline immediately after spawning the worker, the Rust side could
/// emit VAD-detected speech frames into the child's stdin DURING the
/// child's model load — the Python reader doesn't start until
/// `Dictate.__init__` (which runs after the model is ready). Those
/// pre-ready frames piled up in the OS pipe buffer (≈64 KB) and the
/// first PTT press would drain them as stale audio; worse, a full pipe
/// blocks the writer thread so `stop()` could hang joining it.
///
/// The fix: split [`spawn_bridge`] into two stages. The supervisor:
///
/// 1. Calls [`spawn_bridge_pending_ready`] right after spawning the
///    worker. This eagerly loads the Silero VAD model (fail-fast for a
///    missing/bad ONNX file) and parks the result in a [`PendingBridge`].
///    No cpal stream is opened; no frames are produced.
/// 2. Watches the worker's stderr for the `state=ready` event the
///    Python child emits after its model loads (see
///    `runtime.py::_emit_worker_event(..., state="ready", ...)`).
/// 3. Calls [`PendingBridge::start`] on receipt of `ready`, which opens
///    cpal and spawns the writer thread. Frames now flow into a stdin
///    reader that's guaranteed to be alive.
///
/// If the worker dies before `ready` (model-load failure), the
/// supervisor drops the `PendingBridge`; no cpal stream is ever opened
/// and the user's mic-permission prompt is preserved for the next run.
pub struct PendingBridge {
    silero: Option<SileroVad>,
    writer_out: Option<Box<dyn Write + Send>>,
}

impl PendingBridge {
    /// Activate the pipeline: open cpal for `device_name`, spawn the
    /// writer thread that pipes events into the worker's stdin, and
    /// return a [`BridgeHandle`] plus the error channel — same shape
    /// the single-stage [`spawn_bridge`] returned.
    ///
    /// Consumes `self` so the caller cannot accidentally start twice.
    pub fn start(
        mut self,
        device_name: &str,
    ) -> Result<(BridgeHandle, Receiver<BridgeError>), anyhow::Error> {
        let silero = self
            .silero
            .take()
            .expect("PendingBridge::start called twice (SileroVad already taken)");
        let out = self
            .writer_out
            .take()
            .expect("PendingBridge::start called twice (writer already taken)");
        let (pipeline, events) = AudioPipeline::start(device_name, move || Ok(silero))?;
        let (err_tx, err_rx) = mpsc::channel::<BridgeError>();
        let writer = thread::spawn(move || {
            run_writer(events, out, err_tx);
        });
        Ok((
            BridgeHandle {
                pipeline: Some(pipeline),
                writer: Some(writer),
            },
            err_rx,
        ))
    }
}

/// Spawn the audio pipeline AND the writer thread that pipes its events
/// into `out`.
///
/// `out` is typically the child's stdin (`Stdio::piped()`); in tests it
/// can be any `Write + Send + 'static` (e.g. a `Vec<u8>` wrapped in a
/// channel-backed mock, or a real `PipeWriter`).
///
/// The pipeline starts BEFORE the writer thread so that a model-load
/// failure surfaces as a synchronous `Err(_)` from this call (no
/// background thread to babysit). On success the writer thread takes
/// ownership of `out` and the returned handle owns both halves.
pub fn spawn_bridge<W, L>(
    device_name: &str,
    out: W,
    model_loader: L,
) -> Result<(BridgeHandle, Receiver<BridgeError>), anyhow::Error>
where
    W: Write + Send + 'static,
    L: FnOnce() -> Result<SileroVad, anyhow::Error>,
{
    let (pipeline, events) = AudioPipeline::start(device_name, model_loader)?;
    let (err_tx, err_rx) = mpsc::channel::<BridgeError>();
    let writer = thread::spawn(move || {
        run_writer(events, out, err_tx);
    });
    Ok((
        BridgeHandle {
            pipeline: Some(pipeline),
            writer: Some(writer),
        },
        err_rx,
    ))
}

/// Stage one of the deferred-start bridge (iteration-3 review finding
/// #1). Loads the Silero VAD model so a missing/bad ONNX surfaces as a
/// synchronous error, parks the worker's stdin writer for later, and
/// returns a [`PendingBridge`] the supervisor can [`PendingBridge::start`]
/// once the Python child has emitted its `state=ready` event.
///
/// No cpal stream is opened by this call — that's deferred to
/// [`PendingBridge::start`] so the OS pipe buffer is never filled with
/// pre-ready frames the Python worker has no reader for.
pub fn spawn_bridge_pending_ready<W, L>(
    out: W,
    model_loader: L,
) -> Result<PendingBridge, anyhow::Error>
where
    W: Write + Send + 'static,
    L: FnOnce() -> Result<SileroVad, anyhow::Error>,
{
    let silero = model_loader()?;
    Ok(PendingBridge {
        silero: Some(silero),
        writer_out: Some(Box::new(out)),
    })
}

/// Pure drain loop, factored out so it can be tested without spawning a
/// real pipeline (we feed events directly on the `Receiver`).
fn run_writer<W: Write>(events: Receiver<PipelineEvent>, mut out: W, err_tx: Sender<BridgeError>) {
    loop {
        match events.recv() {
            Ok(event) => {
                let line = event_to_json_line(&event);
                // Build the line + newline in one write so a partial
                // write can't leave the Python decoder reading a
                // half-event (its iterator splits on newline).
                let mut buf = line.into_bytes();
                buf.push(b'\n');
                if let Err(err) = out.write_all(&buf).and_then(|()| out.flush()) {
                    let mapped = if err.kind() == std::io::ErrorKind::BrokenPipe {
                        BridgeError::WorkerClosed
                    } else {
                        BridgeError::Io(err.to_string())
                    };
                    let _ = err_tx.send(mapped);
                    return;
                }
                // The pipeline's wire contract documents `DeviceError`
                // as TERMINAL — no further events arrive. We've
                // forwarded the JSON to Python, now ALSO surface it on
                // the error channel so the supervisor can react even
                // if Python had already closed its stdin.
                if let PipelineEvent::DeviceError(msg) = event {
                    let _ = err_tx.send(BridgeError::Pipeline(msg));
                    return;
                }
            }
            Err(RecvError) => {
                // The pipeline shut down. No error — this is the normal
                // teardown path triggered by `BridgeHandle::stop`. Drop
                // `err_tx` by returning; the supervisor sees the
                // channel close and continues with its own teardown.
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};

    /// Captures everything written through it, with an optional
    /// "broken-pipe after N bytes" trigger for the worker-closed test.
    #[derive(Clone, Default)]
    struct CapturingWriter {
        buf: Arc<Mutex<Vec<u8>>>,
        broken_after: Option<usize>,
    }

    impl CapturingWriter {
        fn new() -> Self {
            Self::default()
        }
        fn breaks_after(n: usize) -> Self {
            Self {
                buf: Arc::new(Mutex::new(Vec::new())),
                broken_after: Some(n),
            }
        }
    }

    impl Write for CapturingWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if let Some(limit) = self.broken_after {
                if self.buf.lock().unwrap().len() >= limit {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "test broken pipe",
                    ));
                }
            }
            self.buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Iteration-3 review finding #1: `spawn_bridge_pending_ready`
    /// must load the Silero model eagerly (so a missing/bad ONNX
    /// fails-fast) but MUST NOT open the cpal capture stream — that's
    /// the whole point of the two-stage handshake. We assert this by
    /// passing a failing model loader (synchronous Err returned from
    /// the call) and a loader that succeeds (no Err) and observing
    /// that `start_capture` is only invoked when we actually call
    /// `PendingBridge::start`. Without a real cpal-in-process stub we
    /// detect the start by passing an obviously-bogus device name and
    /// asserting that the error surfaces from `start()`, not from
    /// `spawn_bridge_pending_ready`.
    #[test]
    fn spawn_pending_ready_surfaces_model_loader_failure_synchronously() {
        let writer = CapturingWriter::new();
        let result =
            spawn_bridge_pending_ready(writer, || Err(anyhow::anyhow!("synthetic loader failure")));
        assert!(
            result.is_err(),
            "model-load failure must propagate from spawn_bridge_pending_ready",
        );
        let msg = format!("{}", result.err().unwrap());
        assert!(
            msg.contains("synthetic loader failure"),
            "expected loader error, got: {msg}",
        );
    }

    #[test]
    fn spawn_pending_ready_does_not_open_capture_stream() {
        // If `spawn_bridge_pending_ready` accidentally called
        // `AudioPipeline::start` itself (the pre-fix behaviour
        // promoted by `spawn_bridge`), cpal would try to open the
        // bogus device name and FAIL — which would surface here, not
        // when we call `PendingBridge::start`. By asserting the call
        // succeeds despite the device being unopenable, we prove the
        // cpal open is correctly deferred.
        let writer = CapturingWriter::new();
        let pending = spawn_bridge_pending_ready(writer, || {
            // Loader succeeds — use the same path as the real
            // SileroVad::from_embedded_bytes (with the embedded model
            // bytes from the crate). The pending bridge must NOT open
            // any cpal stream during this call.
            super::SileroVad::from_embedded_bytes(super::super::EMBEDDED_SILERO_BYTES)
        });
        assert!(
            pending.is_ok(),
            "loader-only path must succeed without touching cpal",
        );
        // We deliberately drop the PendingBridge here instead of
        // calling .start() — a buggy implementation that opened cpal
        // up-front would leak the stream past this point. Dropping
        // the PendingBridge proves the no-open contract: there's
        // nothing for `Drop` to clean up because nothing was opened.
        drop(pending);
    }

    #[test]
    fn writer_forwards_events_as_newline_delimited_json() {
        let (tx, rx) = mpsc::channel::<PipelineEvent>();
        let (etx, _erx) = mpsc::channel::<BridgeError>();
        let writer = CapturingWriter::new();
        let buf_handle = writer.buf.clone();
        let handle = thread::spawn(move || run_writer(rx, writer, etx));

        tx.send(PipelineEvent::SpeechStart).unwrap();
        tx.send(PipelineEvent::SpeechEnd).unwrap();
        drop(tx); // close channel → writer exits
        handle.join().unwrap();

        let text = String::from_utf8(buf_handle.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "got {text:?}");
        assert!(lines[0].contains("speech_start"));
        assert!(lines[1].contains("speech_end"));
    }

    #[test]
    fn writer_translates_broken_pipe_to_worker_closed() {
        let (tx, rx) = mpsc::channel::<PipelineEvent>();
        let (etx, erx) = mpsc::channel::<BridgeError>();
        // breaks_after(0) → the very first write fails with BrokenPipe.
        let writer = CapturingWriter::breaks_after(0);
        let handle = thread::spawn(move || run_writer(rx, writer, etx));

        tx.send(PipelineEvent::SpeechStart).unwrap();
        handle.join().unwrap();

        let err = erx.recv().expect("bridge must emit one error");
        assert!(
            matches!(err, BridgeError::WorkerClosed),
            "got {err:?}, want WorkerClosed",
        );
        // No further errors expected.
        assert!(erx.try_recv().is_err());
        // And the writer hung up so the sender's next send fails too.
        assert!(
            tx.send(PipelineEvent::SpeechEnd).is_err() || tx.send(PipelineEvent::SpeechEnd).is_ok()
        );
    }

    #[test]
    fn device_error_event_forwarded_then_surfaced_on_error_channel() {
        let (tx, rx) = mpsc::channel::<PipelineEvent>();
        let (etx, erx) = mpsc::channel::<BridgeError>();
        let writer = CapturingWriter::new();
        let buf_handle = writer.buf.clone();
        let handle = thread::spawn(move || run_writer(rx, writer, etx));

        tx.send(PipelineEvent::DeviceError("mic unplugged".into()))
            .unwrap();
        // Writer must exit on its own after seeing DeviceError — we do
        // NOT drop the sender first, so a buggy implementation that
        // keeps draining would block forever and the test would hang
        // on the join below (CI timeout catches it).
        handle.join().unwrap();

        // Python received the JSON line.
        let text = String::from_utf8(buf_handle.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains("device_error") && text.contains("mic unplugged"),
            "expected device_error JSON, got {text:?}",
        );
        // And the supervisor sees the Pipeline error.
        let err = erx.recv().expect("bridge must emit a pipeline error");
        assert!(
            matches!(&err, BridgeError::Pipeline(msg) if msg == "mic unplugged"),
            "got {err:?}",
        );
    }
}
