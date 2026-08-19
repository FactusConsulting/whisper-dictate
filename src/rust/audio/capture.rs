//! `cpal`-based microphone capture in a dedicated worker thread.
//!
//! The pipeline downstream of this module wants mono `f32` samples at the
//! device's native rate; resampling to 16 kHz happens later (see
//! [`super::resampler`]). Keeping the capture callback minimal — sample
//! format conversion + channel-average mixdown only — leaves enough
//! headroom on slow USB mics to never drop a buffer.
//!
//! Lifecycle:
//! * [`start_capture`] spawns a worker that opens the chosen device,
//!   negotiates a supported config (priority `F32 > I16 > I32`) at the
//!   device's native rate, and starts the stream.
//! * Each callback converts the device buffer to mono `f32` and uses a
//!   non-blocking send into a fixed-capacity, drop-oldest queue.
//! * Setting `stop_flag` to `true` triggers the worker to drop the stream
//!   and push a final [`AudioChunk::EndOfStream`] sentinel so the consumer
//!   knows when it's safe to flush the resampler and shut down.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

use super::bounded_queue::{bounded_latest, LatestReceiver, LatestSender, OverflowMetric};
use super::hosts::{resolve_input, ResolvedInput};

/// Messages sent from the capture worker to the consumer thread.
#[derive(Debug)]
pub enum AudioChunk {
    /// A burst of mono `f32` samples at the device's native sample rate.
    Samples(Vec<f32>),
    /// The capture loop stopped cleanly. Pushed AFTER all in-flight
    /// `Samples` messages so the consumer can drain them and then flush
    /// the resampler without losing the tail of the recording.
    EndOfStream,
    /// The capture loop hit an unrecoverable error. The consumer should
    /// surface this to the user and tear the pipeline down.
    Error(String),
}

const CAPTURE_START_TIMEOUT: Duration = Duration::from_secs(5);

/// Sixteen native-rate bursts cover about 160 ms at the common 10 ms callback
/// cadence. Each queued burst is capped separately, so the queue owns at most
/// 256 KiB of sample payload (`16 * 4096 * sizeof(f32)`) regardless of a
/// backend's callback-buffer size.
pub const CAPTURE_QUEUE_CAPACITY: usize = 16;
const MAX_CAPTURE_CHUNK_SAMPLES: usize = 4096;

#[derive(Clone)]
pub struct AudioChunkSender {
    data_tx: LatestSender<AudioChunk>,
    error_tx: crossbeam_channel::Sender<AudioChunk>,
}

impl AudioChunkSender {
    pub(crate) fn try_send_latest(&self, chunk: AudioChunk) -> Result<bool, AudioChunk> {
        if matches!(chunk, AudioChunk::Error(_)) {
            return self
                .error_tx
                .try_send(chunk)
                .map(|()| false)
                .map_err(|error| match error {
                    crossbeam_channel::TrySendError::Full(chunk)
                    | crossbeam_channel::TrySendError::Disconnected(chunk) => chunk,
                });
        }
        self.data_tx.try_send_latest(chunk)
    }
}

pub struct AudioChunkReceiver {
    data_rx: LatestReceiver<AudioChunk>,
    error_rx: crossbeam_channel::Receiver<AudioChunk>,
    // Keep the control channel connected while the receiver exists. This lets
    // a normal data-channel disconnect wake `recv` without an empty,
    // disconnected error channel winning the biased selection first.
    _error_keepalive: crossbeam_channel::Sender<AudioChunk>,
}

impl AudioChunkReceiver {
    pub fn recv(&self) -> Result<AudioChunk, crossbeam_channel::RecvError> {
        crossbeam_channel::select_biased! {
            recv(self.error_rx) -> chunk => chunk,
            recv(self.data_rx.channel()) -> chunk => chunk,
        }
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<AudioChunk, crossbeam_channel::RecvTimeoutError> {
        crossbeam_channel::select_biased! {
            recv(self.error_rx) -> chunk => chunk.map_err(|_| crossbeam_channel::RecvTimeoutError::Disconnected),
            recv(self.data_rx.channel()) -> chunk => chunk.map_err(|_| crossbeam_channel::RecvTimeoutError::Disconnected),
            default(timeout) => Err(crossbeam_channel::RecvTimeoutError::Timeout),
        }
    }

    pub fn try_recv(&self) -> Result<AudioChunk, crossbeam_channel::TryRecvError> {
        match self.error_rx.try_recv() {
            Ok(chunk) => Ok(chunk),
            Err(crossbeam_channel::TryRecvError::Empty)
            | Err(crossbeam_channel::TryRecvError::Disconnected) => self.data_rx.try_recv(),
        }
    }

    pub(crate) fn overflow_metric(&self) -> OverflowMetric {
        self.data_rx.overflow_metric()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.data_rx.len() + self.error_rx.len()
    }
}

pub fn audio_chunk_channel() -> (AudioChunkSender, AudioChunkReceiver) {
    audio_chunk_channel_with_capacity(CAPTURE_QUEUE_CAPACITY)
}

fn audio_chunk_channel_with_capacity(capacity: usize) -> (AudioChunkSender, AudioChunkReceiver) {
    let (data_tx, data_rx) = bounded_latest(capacity);
    let (error_tx, error_rx) = crossbeam_channel::bounded(1);
    (
        AudioChunkSender {
            data_tx,
            error_tx: error_tx.clone(),
        },
        AudioChunkReceiver {
            data_rx,
            error_rx,
            _error_keepalive: error_tx,
        },
    )
}

#[derive(Debug, PartialEq, Eq)]
enum CaptureReadyError {
    Startup(String),
    WorkerExited,
    TimedOut,
}

impl std::fmt::Display for CaptureReadyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Startup(message) => formatter.write_str(message),
            Self::WorkerExited => {
                formatter.write_str("capture worker exited before the input stream became ready")
            }
            Self::TimedOut => formatter.write_str(
                "timed out waiting for the input stream to become ready after 5 seconds",
            ),
        }
    }
}

impl std::error::Error for CaptureReadyError {}

/// Distinguish the startup timeout whose worker may still be blocked inside
/// the operating-system audio driver. Recovery callers use this to avoid
/// spawning an unbounded sequence of detached workers.
#[cfg(any(all(feature = "whisper-rs-local", feature = "rust-injection"), test))]
pub(crate) fn is_capture_start_timeout(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<CaptureReadyError>(),
        Some(CaptureReadyError::TimedOut)
    )
}

/// Handle to a running capture worker. Drop to stop, or call [`stop`]
/// explicitly to block until the worker has emitted `EndOfStream`.
pub struct CaptureHandle {
    stop_flag: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    /// The native sample rate the worker negotiated, exposed so the
    /// consumer can build the matching [`super::resampler::FrameResampler`].
    sample_rate: u32,
}

impl CaptureHandle {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Signal the worker to stop and wait for it to finish. Idempotent.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start capturing from the named input device. An empty `device_name`
/// selects the system default input. Returns only after the stream has
/// started successfully, with the negotiated native sample rate on its handle.
///
/// The producer side runs on a dedicated thread because cpal's stream
/// callback is invoked from a high-priority audio thread that we must
/// keep extremely short — we do nothing in the callback other than
/// mix-to-mono + send a `Vec<f32>` over the channel.
pub fn start_capture(
    device_name: &str,
    tx: AudioChunkSender,
) -> Result<CaptureHandle, anyhow::Error> {
    let ResolvedInput {
        device,
        host_id: _,
        host_label,
    } = resolve_input(device_name)?;
    // Diagnostic breadcrumb: log which host actually opened. When a saved
    // config value could match on multiple hosts (rare on Windows today
    // where cpal only surfaces WASAPI, common on Linux with Pulse + Alsa
    // stacked) knowing the winning host is the first thing that helps
    // debug a "wrong device" report.
    eprintln!(
        "[audio/capture] opening {:?} on cpal host {host_label}",
        if device_name.is_empty() {
            "<system default>"
        } else {
            device_name
        }
    );

    let supported = pick_config(&device)?;
    let sample_format = supported.sample_format();
    let channels = supported.channels();
    // cpal 0.18 type-aliased SampleRate to a plain `u32`, so the old
    // tuple-struct `.0` accessor is gone — the call returns the rate
    // directly.
    let sample_rate = supported.sample_rate();
    let config: StreamConfig = supported.into();

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_for_worker = stop_flag.clone();
    let terminal_error = Arc::new(AtomicBool::new(false));
    let terminal_for_samples = Arc::clone(&terminal_error);
    let terminal_for_error = Arc::clone(&terminal_error);
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);

    let join = thread::spawn(move || {
        // Build + start the stream INSIDE the worker so the cpal Stream is
        // dropped on the same thread it was created on — required by some
        // host backends (WASAPI in particular).
        let tx_for_cb = tx.clone();
        let tx_for_err = tx.clone();
        let build_result = build_input_stream(
            &device,
            config,
            sample_format,
            channels,
            move |samples| {
                if !terminal_for_samples.load(Ordering::Acquire) {
                    enqueue_samples(&tx_for_cb, samples);
                }
            },
            move |err| {
                enqueue_stream_error(&tx_for_err, &terminal_for_error, err);
            },
        );
        let stream = match build_result {
            Ok(s) => s,
            Err(err) => {
                eprintln!("[audio/capture] build input stream failed: {err}");
                let _ = ready_tx.send(Err(format!("build input stream: {err}")));
                return;
            }
        };
        if !should_start_stream(&stop_for_worker) {
            eprintln!("[audio/capture] startup cancelled before input stream activation");
            return;
        }
        if let Err(err) = signal_stream_start(&ready_tx, || {
            stream
                .play()
                .map_err(|error| format!("start stream: {error}"))
        }) {
            eprintln!("[audio/capture] start input stream failed: {err}");
            return;
        }
        // Park-with-poll loop. We don't need precise wake-up — 10 ms is far
        // shorter than the worst-case capture latency on Windows.
        while !stop_for_worker.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(10));
        }
        // Dropping `stream` here stops the stream cleanly.
        drop(stream);
        enqueue_end_of_stream(&tx, &terminal_error);
    });

    eprintln!("[audio/capture] waiting for input stream startup");
    if let Err(err) = await_capture_ready(ready_rx, CAPTURE_START_TIMEOUT) {
        let timed_out = matches!(&err, CaptureReadyError::TimedOut);
        stop_flag.store(true, Ordering::SeqCst);
        if !timed_out {
            let _ = join.join();
        }
        return Err(anyhow::Error::new(err));
    }
    eprintln!("[audio/capture] input stream ready");
    Ok(CaptureHandle {
        stop_flag,
        join: Some(join),
        sample_rate,
    })
}

/// Split an unusually large backend buffer so every retained queue item has a
/// fixed maximum payload. Every send is non-blocking; when the resampler is
/// behind, the queue evicts its oldest burst and keeps the newest audio.
fn enqueue_samples(tx: &AudioChunkSender, samples: Vec<f32>) {
    if samples.len() <= MAX_CAPTURE_CHUNK_SAMPLES {
        let _ = tx.try_send_latest(AudioChunk::Samples(samples));
        return;
    }
    for chunk in samples.chunks(MAX_CAPTURE_CHUNK_SAMPLES) {
        let _ = tx.try_send_latest(AudioChunk::Samples(chunk.to_vec()));
    }
}

fn enqueue_terminal_error(tx: &AudioChunkSender, terminal: &AtomicBool, message: String) {
    if !terminal.swap(true, Ordering::AcqRel) {
        let _ = tx.try_send_latest(AudioChunk::Error(message));
    }
}

/// CPAL reports buffer xruns through the stream error callback. An xrun loses
/// audio from one callback interval but leaves the stream usable, so treating
/// it as terminal would unnecessarily abort an otherwise active recording.
fn enqueue_stream_error(tx: &AudioChunkSender, terminal: &AtomicBool, error: cpal::Error) {
    if error.kind() == cpal::ErrorKind::Xrun {
        let _ = crate::diag::write_line_nonblocking(&format!(
            "[audio/capture] nonterminal cpal stream xrun: {error}"
        ));
        return;
    }
    enqueue_terminal_error(tx, terminal, format!("cpal stream error: {error}"));
}

fn enqueue_end_of_stream(tx: &AudioChunkSender, terminal: &AtomicBool) {
    if !terminal.load(Ordering::Acquire) {
        let _ = tx.try_send_latest(AudioChunk::EndOfStream);
    }
}

fn await_capture_ready(
    ready_rx: Receiver<Result<(), String>>,
    timeout: Duration,
) -> Result<(), CaptureReadyError> {
    match ready_rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => Err(CaptureReadyError::Startup(message)),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(CaptureReadyError::WorkerExited),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(CaptureReadyError::TimedOut),
    }
}

fn should_start_stream(stop_flag: &AtomicBool) -> bool {
    !stop_flag.load(Ordering::SeqCst)
}

fn signal_stream_start<F>(
    ready_tx: &mpsc::SyncSender<Result<(), String>>,
    start: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    let result = start();
    let signal = result.as_ref().map(|_| ()).map_err(Clone::clone);
    let _ = ready_tx.send(signal);
    result
}

// ----- helpers ----------------------------------------------------------------

/// Outcome of [`resolve_device_index`] — either an index into the
/// enumerated device list or a structured error for the caller to
/// translate.
///
/// **Test-only fixture.** Production callers now go through
/// [`super::hosts::resolve_input`], which does the same lookup pooled
/// across every cpal host. Kept under `#[cfg(test)]` because the tests
/// below encode the per-host precedence contract (exact → longest
/// substring → numeric index) that the multi-host resolver inlines and
/// relies on; removing the fixture would remove that documentation.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeviceLookup {
    /// The selector matched the device at this index.
    Matched(usize),
    /// The selector is a numeric index that's outside the device list.
    IndexOutOfRange { wanted: usize, available: usize },
    /// No exact name, substring or numeric index matched.
    NotFound,
}

/// Resolve a device selector against a list of device names. Pure
/// helper so the lookup precedence (exact → substring → numeric index)
/// can be unit-tested without a live cpal host.
///
/// Precedence:
///   1. Empty selector → caller handles default device.
///   2. Exact case-insensitive match — first hit wins.
///   3. Case-insensitive substring match — first hit wins (sounddevice
///      style; matches `vp_devices.py`).
///   4. Trimmed numeric selector → index into `device_names`.
///
/// Returns [`DeviceLookup::Matched`] with the chosen index on success,
/// [`DeviceLookup::IndexOutOfRange`] when a parseable index is past the
/// end of the list, and [`DeviceLookup::NotFound`] otherwise.
///
/// See the note on [`DeviceLookup`] for why this is `#[cfg(test)]`: the
/// multi-host resolver in `super::hosts::resolve_input` is the sole
/// production caller of this precedence today (inlined so it can pool
/// exact matches across hosts). The tests below pin the per-host
/// contract this fixture documents.
#[cfg(test)]
pub(crate) fn resolve_device_index(device_names: &[String], selector: &str) -> DeviceLookup {
    let needle = selector.trim().to_lowercase();
    // 1. Exact case-insensitive match wins.
    for (idx, name) in device_names.iter().enumerate() {
        if name.to_lowercase() == needle {
            return DeviceLookup::Matched(idx);
        }
    }
    // 2. Bidirectional substring match, keeping the LONGEST device name — the
    //    same precedence as `crate::devices::find_in` (and Python's
    //    `vp_devices._best_match`), so a truncated / generic `--device` value
    //    (e.g. a Windows MME-truncated endpoint name, or a bare "Microphone")
    //    binds to its fullest sibling rather than to whichever shorter match
    //    happens to enumerate first. Either side may be the prefix. An empty
    //    needle never reaches here in production (`pick_device` maps "" to the
    //    default device first); guard it so it can't spuriously match every
    //    device via the empty-substring rule.
    if !needle.is_empty() {
        let mut best: Option<usize> = None;
        for (idx, name) in device_names.iter().enumerate() {
            let lower = name.to_lowercase();
            if lower.is_empty() || !(lower.contains(&needle) || needle.contains(&lower)) {
                continue;
            }
            match best {
                None => best = Some(idx),
                Some(prev) if name.len() > device_names[prev].len() => best = Some(idx),
                _ => {}
            }
        }
        if let Some(idx) = best {
            return DeviceLookup::Matched(idx);
        }
    }
    // 3. Numeric index fallback (capture-specific; `devices::find_in` has none).
    if let Ok(idx) = selector.trim().parse::<usize>() {
        if idx < device_names.len() {
            return DeviceLookup::Matched(idx);
        }
        return DeviceLookup::IndexOutOfRange {
            wanted: idx,
            available: device_names.len(),
        };
    }
    DeviceLookup::NotFound
}

/// Cross-host resolver moved to [`super::hosts::resolve_input`] so the CLI
/// probe (`audio::device_probe`) and live capture pick the same device on
/// the same host. This shim was the single-host default-host-only version
/// that shipped through rc.13 — it silently lost mics reachable via
/// non-default cpal hosts. The new resolver walks `default_host` first,
/// then the rest of `cpal::available_hosts()`. See the hosts module for
/// the DirectSound gap on Windows (cpal 0.18 has no DirectSound host).
fn pick_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, anyhow::Error> {
    // Priority F32 > I16 > I32. We always pick the device's native rate
    // (max_sample_rate of the supported config) and resample later.
    let mut best_f32: Option<cpal::SupportedStreamConfigRange> = None;
    let mut best_i16: Option<cpal::SupportedStreamConfigRange> = None;
    let mut best_i32: Option<cpal::SupportedStreamConfigRange> = None;

    let supported = device
        .supported_input_configs()
        .map_err(|err| anyhow::anyhow!("supported_input_configs: {err}"))?;
    for cfg in supported {
        match cfg.sample_format() {
            SampleFormat::F32 => best_f32 = Some(cfg),
            SampleFormat::I16 => best_i16 = Some(cfg),
            SampleFormat::I32 => best_i32 = Some(cfg),
            _ => {}
        }
    }
    let picked = best_f32
        .or(best_i16)
        .or(best_i32)
        .ok_or_else(|| anyhow::anyhow!("no F32/I16/I32 input config supported"))?;
    // Pick the highest natively-supported rate within the range.
    Ok(picked.with_max_sample_rate())
}

fn build_input_stream<F, E>(
    device: &cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    channels: u16,
    on_samples: F,
    on_error: E,
) -> Result<cpal::Stream, anyhow::Error>
where
    F: FnMut(Vec<f32>) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    // We're paranoid about the audio callback: wrap the user-supplied
    // `on_samples` in a closure that owns the (cheap) mix-to-mono.
    let channels_usize = channels as usize;
    let on_samples = std::sync::Mutex::new(on_samples);
    let on_samples = std::sync::Arc::new(on_samples);

    macro_rules! callback_for {
        ($sample_ty:ty, $to_f32:expr) => {{
            let on_samples = on_samples.clone();
            move |buffer: &[$sample_ty], _: &cpal::InputCallbackInfo| {
                let mono = mix_to_mono(buffer, channels_usize, $to_f32);
                if let Ok(mut cb) = on_samples.try_lock() {
                    cb(mono);
                }
            }
        }};
    }

    // cpal 0.18 unified the stream/build errors under a single
    // `cpal::Error` (the old `StreamError` was removed); the callback
    // signature is `FnMut(cpal::Error)` now.
    let on_error = std::sync::Mutex::new(on_error);
    let on_error = std::sync::Arc::new(on_error);
    let err_cb = move |err: cpal::Error| {
        if let Ok(mut cb) = on_error.try_lock() {
            cb(err);
        }
    };

    // cpal 0.18 takes `StreamConfig` by value (not by ref) and adds an
    // explicit `timeout: Option<Duration>` arg. `None` matches the prior
    // "block indefinitely until the device opens" semantics.
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
            config,
            callback_for!(f32, |s: f32| s),
            err_cb,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream::<i16, _, _>(
            config,
            callback_for!(i16, |s: i16| (s as f32) / (i16::MAX as f32)),
            err_cb,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream::<i32, _, _>(
            config,
            callback_for!(i32, |s: i32| (s as f32) / (i32::MAX as f32)),
            err_cb,
            None,
        ),
        other => {
            return Err(anyhow::anyhow!(
                "unsupported sample format negotiated: {other:?}"
            ));
        }
    };
    stream.map_err(|err| anyhow::anyhow!("build_input_stream: {err}"))
}

/// Channel-average mix to mono. Pure / no `cfg` so it can be unit tested
/// on every build. The `to_f32` closure normalises integer samples into
/// the `[-1.0, 1.0]` range and is a no-op for native f32 buffers.
pub fn mix_to_mono<T, F>(buffer: &[T], channels: usize, to_f32: F) -> Vec<f32>
where
    T: Copy,
    F: Fn(T) -> f32,
{
    if channels <= 1 {
        return buffer.iter().copied().map(&to_f32).collect();
    }
    let frames = buffer.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for frame_idx in 0..frames {
        let start = frame_idx * channels;
        let mut sum = 0.0_f32;
        for ch in 0..channels {
            sum += to_f32(buffer[start + ch]);
        }
        out.push(sum / channels as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn oversized_callback_buffer_is_split_into_fixed_payloads() {
        let (tx, rx) = audio_chunk_channel_with_capacity(2);
        enqueue_samples(&tx, vec![0.25; MAX_CAPTURE_CHUNK_SAMPLES + 3]);

        match rx.recv().expect("first chunk") {
            AudioChunk::Samples(samples) => {
                assert_eq!(samples.len(), MAX_CAPTURE_CHUNK_SAMPLES)
            }
            other => panic!("expected samples, got {other:?}"),
        }
        match rx.recv().expect("second chunk") {
            AudioChunk::Samples(samples) => assert_eq!(samples.len(), 3),
            other => panic!("expected samples, got {other:?}"),
        }
        assert_eq!(rx.overflow_metric().count(), 0);
    }

    #[test]
    fn oversized_burst_retains_newest_bounded_chunks() {
        let (tx, rx) = audio_chunk_channel_with_capacity(2);
        let samples: Vec<f32> = (0..(MAX_CAPTURE_CHUNK_SAMPLES * 3))
            .map(|value| value as f32)
            .collect();
        enqueue_samples(&tx, samples);

        assert_eq!(rx.len(), 2);
        assert_eq!(rx.overflow_metric().count(), 1);
        match rx.recv().expect("newer chunk") {
            AudioChunk::Samples(samples) => {
                assert_eq!(samples[0], MAX_CAPTURE_CHUNK_SAMPLES as f32)
            }
            other => panic!("expected samples, got {other:?}"),
        }
    }

    #[test]
    fn terminal_error_cannot_be_replaced_by_eos_or_repeated_error() {
        let (tx, rx) = audio_chunk_channel_with_capacity(1);
        let terminal = AtomicBool::new(false);

        enqueue_terminal_error(&tx, &terminal, "device lost".to_owned());
        enqueue_terminal_error(&tx, &terminal, "duplicate".to_owned());
        enqueue_end_of_stream(&tx, &terminal);

        match rx.recv().expect("terminal error") {
            AudioChunk::Error(message) => assert_eq!(message, "device lost"),
            other => panic!("expected terminal error, got {other:?}"),
        }
        assert!(matches!(
            rx.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty)
        ));
    }

    #[test]
    fn xrun_is_nonterminal_and_capture_continues() {
        let (tx, rx) = audio_chunk_channel_with_capacity(1);
        let terminal = AtomicBool::new(false);

        enqueue_stream_error(&tx, &terminal, cpal::Error::new(cpal::ErrorKind::Xrun));
        enqueue_samples(&tx, vec![0.25, 0.5]);

        assert!(
            !terminal.load(Ordering::Acquire),
            "a transient xrun must not stop the capture callback"
        );
        match rx.recv().expect("samples after xrun") {
            AudioChunk::Samples(samples) => assert_eq!(samples, vec![0.25, 0.5]),
            other => panic!("expected samples after xrun, got {other:?}"),
        }
        assert!(matches!(
            rx.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty)
        ));
    }

    #[test]
    fn non_xrun_stream_error_remains_terminal() {
        let (tx, rx) = audio_chunk_channel_with_capacity(1);
        let terminal = AtomicBool::new(false);

        enqueue_stream_error(
            &tx,
            &terminal,
            cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable),
        );

        assert!(terminal.load(Ordering::Acquire));
        match rx.recv().expect("terminal error") {
            AudioChunk::Error(message) => assert!(message.contains("device")),
            other => panic!("expected terminal error, got {other:?}"),
        }
    }

    #[test]
    fn in_flight_sample_flood_cannot_evict_terminal_error() {
        let (tx, rx) = audio_chunk_channel_with_capacity(2);
        let terminal = Arc::new(AtomicBool::new(false));
        let past_terminal_check = Arc::new(Barrier::new(2));
        let error_published = Arc::new(Barrier::new(2));

        let sample_tx = tx.clone();
        let sample_terminal = Arc::clone(&terminal);
        let sample_past_check = Arc::clone(&past_terminal_check);
        let sample_error_published = Arc::clone(&error_published);
        let sample = thread::spawn(move || {
            // Reproduce the production race: this callback passes its one
            // terminal check before the error callback publishes the error.
            assert!(!sample_terminal.load(Ordering::Acquire));
            sample_past_check.wait();
            sample_error_published.wait();
            enqueue_samples(&sample_tx, vec![0.25; MAX_CAPTURE_CHUNK_SAMPLES * 8]);
        });

        past_terminal_check.wait();
        enqueue_terminal_error(&tx, &terminal, "device lost".to_owned());
        error_published.wait();
        sample.join().expect("join in-flight callback");

        match rx.recv().expect("prioritized terminal error") {
            AudioChunk::Error(message) => assert_eq!(message, "device lost"),
            other => panic!("expected terminal error, got {other:?}"),
        }
        assert_eq!(rx.len(), 2, "sample flood remains bounded");
        assert!(
            rx.overflow_metric().count() > 0,
            "fixture must overflow the lossy sample queue"
        );
    }

    #[test]
    fn capture_start_waits_for_the_ready_signal() {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let started = std::time::Instant::now();
        let signaler = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            ready_tx.send(Ok(())).expect("send ready");
        });

        assert!(await_capture_ready(ready_rx, Duration::from_secs(1)).is_ok());
        assert!(started.elapsed() >= Duration::from_millis(15));
        signaler.join().expect("join ready signaler");
    }

    #[test]
    fn capture_start_returns_the_stream_start_error() {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        ready_tx
            .send(Err("start stream: access denied".to_owned()))
            .expect("send startup error");

        assert_eq!(
            await_capture_ready(ready_rx, Duration::ZERO)
                .expect_err("startup failure must be returned")
                .to_string(),
            "start stream: access denied"
        );
    }

    #[test]
    fn capture_start_reports_a_worker_that_exits_without_signalling() {
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        drop(ready_tx);

        assert!(await_capture_ready(ready_rx, Duration::ZERO)
            .expect_err("a missing ready signal must fail startup")
            .to_string()
            .contains("before the input stream became ready"));
    }

    #[test]
    fn capture_start_times_out_when_the_worker_never_signals() {
        let (_ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        assert_eq!(
            await_capture_ready(ready_rx, Duration::ZERO),
            Err(CaptureReadyError::TimedOut)
        );
    }

    #[test]
    fn capture_start_timeout_remains_classifiable_through_anyhow() {
        let error = anyhow::Error::new(CaptureReadyError::TimedOut);
        assert!(is_capture_start_timeout(&error));

        let startup = anyhow::Error::new(CaptureReadyError::Startup("access denied".to_owned()));
        assert!(!is_capture_start_timeout(&startup));
    }

    #[test]
    fn cancelled_capture_does_not_activate_the_stream() {
        let stop_flag = AtomicBool::new(true);

        assert!(!should_start_stream(&stop_flag));
    }

    #[test]
    fn active_capture_starts_the_stream() {
        let stop_flag = AtomicBool::new(false);

        assert!(should_start_stream(&stop_flag));
    }

    #[test]
    fn stream_start_notifies_the_waiter_of_success_and_failure() {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        assert!(signal_stream_start(&ready_tx, || Ok(())).is_ok());
        assert_eq!(ready_rx.recv().expect("receive ready"), Ok(()));

        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        assert_eq!(
            signal_stream_start(&ready_tx, || Err("start stream: WASAPI denied".to_owned())),
            Err("start stream: WASAPI denied".to_owned())
        );
        assert_eq!(
            ready_rx.recv().expect("receive startup error"),
            Err("start stream: WASAPI denied".to_owned())
        );
    }

    #[test]
    fn stream_start_tolerates_a_cancelled_readiness_waiter() {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        drop(ready_rx);

        assert!(signal_stream_start(&ready_tx, || Ok(())).is_ok());
    }

    #[test]
    fn capture_ready_errors_explain_the_failure() {
        assert_eq!(
            CaptureReadyError::TimedOut.to_string(),
            "timed out waiting for the input stream to become ready after 5 seconds"
        );
    }

    #[test]
    fn mono_passthrough_keeps_samples_unchanged() {
        let buf: Vec<f32> = vec![0.1, -0.2, 0.3, -0.4];
        let out = mix_to_mono(&buf, 1, |s: f32| s);
        assert_eq!(out, buf);
    }

    #[test]
    fn stereo_is_averaged_per_frame() {
        // Interleaved L, R, L, R, ...
        let buf: Vec<f32> = vec![0.1, 0.3, -0.4, 0.4];
        let out = mix_to_mono(&buf, 2, |s: f32| s);
        // Frame 0: (0.1 + 0.3) / 2 = 0.2
        // Frame 1: (-0.4 + 0.4) / 2 = 0.0
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.2).abs() < 1e-6);
        assert!(out[1].abs() < 1e-6);
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_device_index_prefers_exact_match_over_substring() {
        let devs = names(&["Realtek HD Audio Mic", "Realtek Mic"]);
        // "realtek mic" exactly matches index 1 (case-insensitive),
        // even though it'd also substring-match index 0.
        assert_eq!(
            resolve_device_index(&devs, "realtek mic"),
            DeviceLookup::Matched(1)
        );
    }

    #[test]
    fn resolve_device_index_substring_match_when_no_exact() {
        let devs = names(&["Realtek HD Audio Mic", "Webcam Mic"]);
        assert_eq!(
            resolve_device_index(&devs, "webcam"),
            DeviceLookup::Matched(1)
        );
    }

    #[test]
    fn resolve_device_index_prefers_longest_bidirectional_substring() {
        // Two generic endpoints both substring-match "usb mic"; the resolver
        // must bind to the LONGEST name (index 1), not the first enumeration
        // hit — matching `devices::find_in` and the Python `_best_match`.
        let devs = names(&["USB Mic (Front)", "USB Mic (Rear Panel Connector)"]);
        assert_eq!(
            resolve_device_index(&devs, "usb mic"),
            DeviceLookup::Matched(1)
        );
        // Truncated saved value (Windows MME caps names at 31 chars): the
        // selector is a prefix of the fuller device name AND a superstring of
        // the bare "Microphone" — the bidirectional match must still bind to
        // the fullest sibling (index 1), not the shorter generic one.
        let devs = names(&["Microphone", "Microphone (High Definition Audio)"]);
        assert_eq!(
            resolve_device_index(&devs, "Microphone (High Definition"),
            DeviceLookup::Matched(1)
        );
    }

    #[test]
    fn resolve_device_index_numeric_selector_indexes_into_list() {
        let devs = names(&["Mic A", "Mic B", "Mic C"]);
        // No name "2" exists, so we fall through to the numeric pass.
        assert_eq!(resolve_device_index(&devs, "2"), DeviceLookup::Matched(2));
        // Leading/trailing whitespace is trimmed before parsing.
        assert_eq!(resolve_device_index(&devs, " 1 "), DeviceLookup::Matched(1));
    }

    #[test]
    fn resolve_device_index_numeric_selector_out_of_range_returns_error() {
        let devs = names(&["Mic A", "Mic B"]);
        assert_eq!(
            resolve_device_index(&devs, "7"),
            DeviceLookup::IndexOutOfRange {
                wanted: 7,
                available: 2
            }
        );
    }

    #[test]
    fn resolve_device_index_numeric_substring_match_wins_over_index_fallback() {
        // If a device literally has "2" in its name, substring match
        // catches it before we'd try the numeric index path.
        let devs = names(&["Mic 2 — USB", "Mic A", "Mic B"]);
        assert_eq!(resolve_device_index(&devs, "2"), DeviceLookup::Matched(0));
    }

    #[test]
    fn resolve_device_index_unknown_selector_returns_not_found() {
        let devs = names(&["Mic A"]);
        assert_eq!(
            resolve_device_index(&devs, "nonexistent"),
            DeviceLookup::NotFound
        );
    }

    #[test]
    fn integer_mixdown_normalises_to_unit_range() {
        // 4 frames stereo @ i16: full-positive on L, full-negative on R.
        let buf: Vec<i16> = vec![i16::MAX, i16::MIN, i16::MAX, i16::MIN];
        let out = mix_to_mono(&buf, 2, |s: i16| (s as f32) / (i16::MAX as f32));
        // Per frame: (1.0 + ~-1.0) / 2 ≈ 0.0 (off by 1 LSB on i16 MIN).
        for &s in &out {
            assert!(s.abs() < 0.001, "frame mixed to ~0 but got {s}");
        }
    }
}
