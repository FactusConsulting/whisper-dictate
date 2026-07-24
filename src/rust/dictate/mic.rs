//! `whisper-dictate dictate-mic` — live microphone capture through the Rust
//! audio pipeline, driving the in-process Rust [`DictateSession`].
//!
//! This is the fully-Rust, no-Python live-capture counterpart of
//! `simulate-session` (which reads a WAV): it opens the mic via the VAD-free
//! [`crate::audio::raw::RawCapturePipeline`] (cpal → rubato → 16 kHz frames),
//! records for a fixed window, then feeds the captured PCM through the SAME
//! cloud-STT + reloading-dictionary + preview-inject session
//! `simulate-session` builds (via [`crate::dictate::simulate::build_cloud_preview_session`]),
//! so the two verbs differ ONLY in the audio source. Injection is
//! preview-only — nothing is typed into the OS.
//!
//! Gated behind the `audio-capture` cargo feature (cpal + rubato, no ONNX);
//! the stock-build stub lives in `main.rs`.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::audio::raw::RawCapturePipeline;
use crate::audio::PipelineEvent;
use crate::dictate::simulate::{
    build_cloud_preview_session, drive_session_over_pcm, simulate_session_config, to_clean_jsonl,
};
use crate::dictate::UtteranceOutcome;

/// Upper bound on `--seconds`. A dictation window is a handful of seconds; an
/// hour is already absurd. The cap also keeps the value well inside what
/// `Duration::from_secs_f64` accepts, so a huge/`inf` input surfaces as the
/// clean `anyhow!` error below instead of panicking the conversion.
const MAX_SECONDS: f64 = 3600.0;

/// The `capture_backend` label stamped into the worker events for the live-mic
/// verb, mirroring `VOICEPI_AUDIO_BACKEND=rust` (the Python metadata uses the
/// mechanism name, e.g. `sounddevice` / `arecord`).
const CAPTURE_BACKEND: &str = "rust";

/// Fold a batch of captured pipeline events into a flat 16 kHz PCM buffer.
///
/// Every [`PipelineEvent::Frame`] is concatenated in order; a
/// [`PipelineEvent::DeviceError`] aborts with that message (a capture failure
/// must not be transcribed as if it were silence). The VAD lifecycle events
/// (`SpeechStart` / `SpeechEnd` / `Cancelled`) are ignored — the VAD-free pump
/// never emits them, but tolerating them keeps this forward-compatible if a
/// caller ever feeds a VAD pipeline's events through here. Pure (no cpal), so
/// it is unit-tested without a real device.
fn frames_to_pcm(events: &[PipelineEvent]) -> Result<Vec<f32>> {
    let mut pcm = Vec::new();
    for event in events {
        match event {
            PipelineEvent::Frame(samples) => pcm.extend_from_slice(samples),
            PipelineEvent::DeviceError(message) => {
                return Err(anyhow!("microphone capture failed: {message}"));
            }
            PipelineEvent::SpeechStart | PipelineEvent::SpeechEnd | PipelineEvent::Cancelled => {}
        }
    }
    Ok(pcm)
}

/// Drain the pipeline's event receiver for `window`, returning the events
/// captured in that window. Stops early if the pipeline hangs up (sender
/// dropped) or emits a terminal [`PipelineEvent::DeviceError`] (which is
/// included as the last event so the caller can surface it). Split from
/// [`capture_pcm_for`] so the time-bounded drain is separate from opening cpal.
fn drain_events_for(rx: &Receiver<PipelineEvent>, window: Duration) -> Vec<PipelineEvent> {
    let deadline = Instant::now() + window;
    let mut events = Vec::new();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(event) => {
                let terminal = matches!(event, PipelineEvent::DeviceError(_));
                events.push(event);
                if terminal {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    events
}

/// Collect every event still queued after the pipeline has been stopped, until
/// the pump drops the sender (channel disconnect) or a terminal `DeviceError`.
/// [`RawCapturePipeline::stop`] joins the pump thread, which flushes the
/// resampler's buffered tail frames onto the channel before exiting — so this
/// post-stop drain recovers the trailing audio that would otherwise be clipped
/// off the end of the recording. `recv` cannot block indefinitely here: the
/// sender is already dropped by the time `stop` returns.
fn drain_remaining(rx: &Receiver<PipelineEvent>) -> Vec<PipelineEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.recv() {
        let terminal = matches!(event, PipelineEvent::DeviceError(_));
        events.push(event);
        if terminal {
            break;
        }
    }
    events
}

/// Open `device` via the VAD-free capture pipeline, record for `seconds`, and
/// return the captured 16 kHz mono PCM. Errors if `seconds` is out of range,
/// the device cannot be opened, or the capture thread reports a `DeviceError`.
fn capture_pcm_for(device: &str, seconds: f64) -> Result<Vec<f32>> {
    // Reject non-finite / non-positive / absurd values BEFORE
    // `Duration::from_secs_f64`, which *panics* (not `Err`) on infinite or
    // overflowing input. `is_finite()` also rejects NaN.
    if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_SECONDS {
        return Err(anyhow!(
            "--seconds must be between 0 and {MAX_SECONDS} (got {seconds})"
        ));
    }
    let (mut pipeline, rx) = RawCapturePipeline::start(device)
        .map_err(|e| anyhow!("open capture device {device:?}: {e:#}"))?;
    let mut events = drain_events_for(&rx, Duration::from_secs_f64(seconds));
    // Stop the stream + join the pump so the mic is released promptly, THEN
    // drain the tail frames the resampler flush emitted during teardown (they
    // land on the channel only after `stop` triggers EndOfStream).
    pipeline.stop();
    events.extend(drain_remaining(&rx));
    frames_to_pcm(&events)
}

/// CLI entry: capture live mic audio for `seconds` and drive one utterance
/// through the cloud-STT preview session. With `json`, the session's worker
/// events are streamed as JSONL; otherwise the injected transcript (or a
/// no-text diagnostic) is printed.
pub fn handle_dictate_mic(device: &str, seconds: f64, json: bool) -> Result<()> {
    // Start from the shared env-sourced config, then stamp in the live-capture
    // metadata so `--json` worker events name the Rust backend + selected mic
    // (an empty `--device` selects the host default input).
    let mut config = simulate_session_config();
    config.capture_backend = CAPTURE_BACKEND.to_owned();
    config.audio_device = if device.is_empty() {
        "default".to_owned()
    } else {
        device.to_owned()
    };
    // Build the session FIRST so a missing cloud key fails fast, before we
    // open the mic / spend the recording window.
    let mut session = build_cloud_preview_session(config)?;
    let pcm = capture_pcm_for(device, seconds)?;

    let outcome = if json {
        if !crate::dictate::env_gates::is_truthy(
            std::env::var("VOICEPI_WORKER_EVENTS").ok().as_deref(),
        ) {
            std::env::set_var("VOICEPI_WORKER_EVENTS", "1");
        }
        let mut buf = Vec::new();
        let outcome = drive_session_over_pcm(&mut session, &pcm, &mut buf)?;
        let jsonl = to_clean_jsonl(&String::from_utf8_lossy(&buf));
        if !jsonl.is_empty() {
            println!("{jsonl}");
        }
        outcome
    } else {
        let mut sink = std::io::sink();
        drive_session_over_pcm(&mut session, &pcm, &mut sink)?
    };

    if !json {
        match outcome {
            UtteranceOutcome::Injected { text, .. } => println!("{text}"),
            other => eprintln!("[dictate-mic] no injection ({other:?})"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_to_pcm_concatenates_frames_in_order() {
        let events = vec![
            PipelineEvent::Frame(vec![0.1, 0.2]),
            PipelineEvent::SpeechStart,
            PipelineEvent::Frame(vec![0.3]),
            PipelineEvent::SpeechEnd,
        ];
        assert_eq!(frames_to_pcm(&events).unwrap(), vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn frames_to_pcm_errors_on_device_error() {
        let events = vec![
            PipelineEvent::Frame(vec![0.1]),
            PipelineEvent::DeviceError("mic unplugged".to_owned()),
        ];
        let err = frames_to_pcm(&events).unwrap_err().to_string();
        assert!(err.contains("mic unplugged"), "{err}");
    }

    #[test]
    fn frames_to_pcm_empty_is_no_samples() {
        assert!(frames_to_pcm(&[]).unwrap().is_empty());
    }

    #[test]
    fn capture_pcm_for_rejects_out_of_range_seconds() {
        // Guard trips before any device is opened, so this is hermetic. Covers
        // non-positive, NaN, and — the panic-avoidance cases — infinite and
        // finite-but-overflowing values that `Duration::from_secs_f64` would
        // otherwise panic on rather than returning `Err`.
        for bad in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e300,
            MAX_SECONDS + 1.0,
        ] {
            let err = capture_pcm_for("", bad).unwrap_err().to_string();
            assert!(err.contains("--seconds"), "got: {err} for {bad}");
        }
    }

    #[test]
    fn drain_remaining_collects_tail_until_disconnect() {
        // Simulates the resampler-flush tail frames that land on the channel
        // after `stop`: pre-load them, drop the sender, and confirm the drain
        // recovers all of them (so trailing audio isn't clipped).
        let (tx, rx) = std::sync::mpsc::channel::<PipelineEvent>();
        tx.send(PipelineEvent::Frame(vec![0.1])).unwrap();
        tx.send(PipelineEvent::Frame(vec![0.2])).unwrap();
        drop(tx);
        assert_eq!(
            drain_remaining(&rx),
            vec![
                PipelineEvent::Frame(vec![0.1]),
                PipelineEvent::Frame(vec![0.2]),
            ],
        );
    }

    #[test]
    fn drain_remaining_stops_at_device_error() {
        let (tx, rx) = std::sync::mpsc::channel::<PipelineEvent>();
        tx.send(PipelineEvent::Frame(vec![0.1])).unwrap();
        tx.send(PipelineEvent::DeviceError("late boom".to_owned()))
            .unwrap();
        tx.send(PipelineEvent::Frame(vec![0.2])).unwrap();
        assert_eq!(
            drain_remaining(&rx),
            vec![
                PipelineEvent::Frame(vec![0.1]),
                PipelineEvent::DeviceError("late boom".to_owned()),
            ],
        );
    }

    #[test]
    fn drain_events_for_returns_promptly_when_sender_hangs_up() {
        // A dropped sender must end the drain immediately rather than blocking
        // for the whole window.
        let (tx, rx) = std::sync::mpsc::channel::<PipelineEvent>();
        tx.send(PipelineEvent::Frame(vec![0.5])).unwrap();
        drop(tx);
        let start = Instant::now();
        let events = drain_events_for(&rx, Duration::from_secs(30));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must not block the full window"
        );
        assert_eq!(events, vec![PipelineEvent::Frame(vec![0.5])]);
    }

    #[test]
    fn drain_events_for_stops_at_device_error() {
        let (tx, rx) = std::sync::mpsc::channel::<PipelineEvent>();
        tx.send(PipelineEvent::Frame(vec![0.1])).unwrap();
        tx.send(PipelineEvent::DeviceError("boom".to_owned()))
            .unwrap();
        tx.send(PipelineEvent::Frame(vec![0.2])).unwrap();
        // sender stays alive; the drain must still stop at the DeviceError.
        let events = drain_events_for(&rx, Duration::from_secs(30));
        assert_eq!(
            events,
            vec![
                PipelineEvent::Frame(vec![0.1]),
                PipelineEvent::DeviceError("boom".to_owned()),
            ],
        );
    }
}
