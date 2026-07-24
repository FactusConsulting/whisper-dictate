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
    build_cloud_preview_session, drive_session_over_pcm, to_clean_jsonl,
};
use crate::dictate::UtteranceOutcome;

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

/// Open `device` via the VAD-free capture pipeline, record for `seconds`, and
/// return the captured 16 kHz mono PCM. Errors if the device cannot be opened
/// or the capture thread reports a `DeviceError`.
fn capture_pcm_for(device: &str, seconds: f64) -> Result<Vec<f32>> {
    if seconds <= 0.0 || seconds.is_nan() {
        return Err(anyhow!("--seconds must be greater than 0 (got {seconds})"));
    }
    let (mut pipeline, rx) = RawCapturePipeline::start(device)
        .map_err(|e| anyhow!("open capture device {device:?}: {e:#}"))?;
    let events = drain_events_for(&rx, Duration::from_secs_f64(seconds));
    // Stop the stream + join the pump before we transcribe so the mic is
    // released promptly and no late frames race the teardown.
    pipeline.stop();
    frames_to_pcm(&events)
}

/// CLI entry: capture live mic audio for `seconds` and drive one utterance
/// through the cloud-STT preview session. With `json`, the session's worker
/// events are streamed as JSONL; otherwise the injected transcript (or a
/// no-text diagnostic) is printed.
pub fn handle_dictate_mic(device: &str, seconds: f64, json: bool) -> Result<()> {
    // Build the session FIRST so a missing cloud key fails fast, before we
    // open the mic / spend the recording window.
    let mut session = build_cloud_preview_session()?;
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
    fn capture_pcm_for_rejects_nonpositive_seconds() {
        // Guard trips before any device is opened, so this is hermetic.
        for bad in [0.0, -1.0, f64::NAN] {
            let err = capture_pcm_for("", bad).unwrap_err().to_string();
            assert!(err.contains("--seconds"), "got: {err}");
        }
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
