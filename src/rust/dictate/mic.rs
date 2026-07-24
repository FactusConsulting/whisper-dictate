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

/// Upper bound on `--seconds`. A dictation window is a handful of seconds, so
/// 5 minutes is already very generous. Kept deliberately small because the
/// captured audio is held in memory three times over (per-frame event vecs →
/// flat PCM in `frames_to_pcm` → the session's own copy): at 16 kHz `f32` that
/// is ~1.9 MB/s per copy, so this cap bounds peak capture memory to a few tens
/// of MB rather than the ~440 MB a full hour would reach. It also keeps the
/// value well inside what `Duration::from_secs_f64` accepts, so a huge / `inf`
/// input surfaces as the clean `anyhow!` error below instead of panicking.
const MAX_SECONDS: f64 = 300.0;

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

/// Validate the requested `--seconds` window against both the hard
/// `Duration::from_secs_f64` limits and the session's effective minimum.
///
/// - Non-finite / non-positive / `> MAX_SECONDS` are rejected BEFORE
///   `Duration::from_secs_f64`, which *panics* (not `Err`) on infinite or
///   overflowing input. `is_finite()` also rejects `NaN`.
/// - A window below the session's effective minimum (`min_record_s` clamped up
///   to [`crate::dictate::skip::MIN_RECORD_FLOOR_S`]) is rejected too: the skip
///   gate would otherwise drop the clip as `too_short` and the verb would exit
///   0 having transcribed nothing. Pure, so it is unit-tested without a device.
fn validate_capture_seconds(seconds: f64, min_record_s: f64) -> Result<()> {
    if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_SECONDS {
        return Err(anyhow!(
            "--seconds must be between 0 and {MAX_SECONDS} (got {seconds})"
        ));
    }
    let floor = min_record_s.max(crate::dictate::skip::MIN_RECORD_FLOOR_S);
    if seconds < floor {
        return Err(anyhow!(
            "--seconds {seconds} is below the session minimum of {floor}s; the \
             capture would be dropped as too short (raise --seconds or lower \
             min_record_seconds)"
        ));
    }
    Ok(())
}

/// Open `device` via the VAD-free capture pipeline, record for `seconds`, and
/// return the captured 16 kHz mono PCM. Errors if `seconds` is out of range /
/// below the session floor, the device cannot be opened, or the capture thread
/// reports a `DeviceError`.
fn capture_pcm_for(device: &str, seconds: f64, min_record_s: f64) -> Result<Vec<f32>> {
    validate_capture_seconds(seconds, min_record_s)?;
    // Apply the PipeWire quantum mitigation (v1.20.6) BEFORE opening cpal, just
    // as `audio::self_test` does: on affected DMIC/PipeWire Linux systems a
    // negotiated 4096-sample quantum starves the cpal callback and the mic
    // never delivers frames. No-op off Linux / when the operator set
    // `PIPEWIRE_QUANTUM` explicitly.
    let _ = crate::audio::pipewire::configure_pipewire_env();
    let (mut pipeline, rx) = RawCapturePipeline::start(device)
        .map_err(|e| anyhow!("open capture device {device:?}: {e:#}"))?;
    let mut events = drain_events_for(&rx, Duration::from_secs_f64(seconds));
    // Stop the stream + join the pump so the mic is released promptly, THEN
    // drain the tail frames the resampler flush emitted during teardown (they
    // land on the channel only after `stop` triggers EndOfStream).
    pipeline.stop();
    events.extend(drain_remaining(&rx));
    nonempty_pcm(&events, device, seconds)
}

/// Fold the captured events into PCM and reject a zero-sample capture. A stream
/// that opens but whose callback never fires yields an empty buffer, which the
/// session would treat as `NoAudio` and the CLI would then exit 0 on — masking
/// a real capture failure. Like `audio::self_test`, treat "no frames delivered"
/// as an error so `dictate-mic` exits non-zero. Split out (pure) so it is
/// unit-tested without opening a device.
fn nonempty_pcm(events: &[PipelineEvent], device: &str, seconds: f64) -> Result<Vec<f32>> {
    let pcm = frames_to_pcm(events)?;
    if pcm.is_empty() {
        return Err(anyhow!(
            "no audio captured from device {device:?} in {seconds}s: the input \
             stream opened but delivered no samples (check the mic is unmuted \
             and not held by another app)"
        ));
    }
    Ok(pcm)
}

/// Print a live `recording` status line (clean JSONL) and flush stdout, so a
/// `--json` supervisor observes `state=recording` at the START of the capture
/// window rather than only after transcription finishes. Emitted in addition to
/// the session's own buffered `opening`/`recording` events (which describe the
/// later transcription phase).
///
/// Routed through [`crate::dictate::events::emit_status`] (the same ASCII-safe
/// `AsciiFormatter` the session's worker events use) rather than a raw
/// `serde_json` write, so a localized `audio_device` with non-ASCII characters
/// is `\uXXXX`-escaped and survives legacy Windows code pages / captured
/// subprocess pipes. `emit_status` honours the `VOICEPI_WORKER_EVENTS` gate, so
/// the caller must enable it first; the `[worker-event] ` prefix is stripped to
/// keep `--json` output valid JSONL.
fn emit_live_recording_status(audio_device: &str, seconds: f64) {
    use crate::dictate::events::{emit_status, StatusEvent, WorkerStatus};
    let mut event = StatusEvent::new(WorkerStatus::Recording);
    event.capture_backend = Some(CAPTURE_BACKEND.to_owned());
    event.audio_device = Some(audio_device.to_owned());
    event
        .extras
        .insert("requested_seconds".into(), serde_json::json!(seconds));
    let mut buf = Vec::new();
    let _ = emit_status(&mut buf, &event);
    let jsonl = to_clean_jsonl(&String::from_utf8_lossy(&buf));
    if !jsonl.is_empty() {
        println!("{jsonl}");
        // Flush so the supervisor sees it before we block in the capture window.
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
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
    let audio_device = if device.is_empty() {
        "default".to_owned()
    } else {
        device.to_owned()
    };
    config.audio_device = audio_device.clone();
    let min_record_s = config.min_record_seconds;
    // Build the session FIRST so a missing cloud key fails fast, before we
    // open the mic / spend the recording window.
    let mut session = build_cloud_preview_session(config)?;

    // Under `--json`, the session's own events are buffered until AFTER the
    // capture completes (the drive transcribes a finished PCM buffer), so a
    // supervisor watching for `state=recording` would otherwise never see it
    // during the actual recording window. Enable the worker-event gate and emit
    // a live `recording` status NOW, before we open the mic, so the consumer
    // can prompt the user in time. The gate must be set before the emit because
    // `events::emit_status` honours it (and the session drive below reuses it).
    if json {
        if !crate::dictate::env_gates::is_truthy(
            std::env::var("VOICEPI_WORKER_EVENTS").ok().as_deref(),
        ) {
            std::env::set_var("VOICEPI_WORKER_EVENTS", "1");
        }
        emit_live_recording_status(&audio_device, seconds);
    }
    let pcm = capture_pcm_for(device, seconds, min_record_s)?;

    let outcome = if json {
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
    fn validate_capture_seconds_rejects_out_of_range() {
        // Covers non-positive, NaN, and — the panic-avoidance cases — infinite
        // and finite-but-overflowing values that `Duration::from_secs_f64`
        // would otherwise panic on rather than returning `Err`.
        for bad in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e300,
            MAX_SECONDS + 1.0,
        ] {
            let err = validate_capture_seconds(bad, 0.5).unwrap_err().to_string();
            assert!(err.contains("between 0"), "got: {err} for {bad}");
        }
    }

    #[test]
    fn validate_capture_seconds_rejects_below_session_floor() {
        // Default 0.5s min-record → a 0.1s window is a guaranteed too_short skip.
        let err = validate_capture_seconds(0.1, 0.5).unwrap_err().to_string();
        assert!(err.contains("below the session minimum"), "got: {err}");
        // Even with min_record=0 the absolute 0.3s skip floor applies.
        let err = validate_capture_seconds(0.2, 0.0).unwrap_err().to_string();
        assert!(err.contains("below the session minimum"), "got: {err}");
        // At/above the effective floor is accepted.
        assert!(validate_capture_seconds(0.5, 0.5).is_ok());
        assert!(validate_capture_seconds(0.3, 0.0).is_ok());
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
    fn nonempty_pcm_rejects_zero_sample_capture() {
        // Stream opened but delivered nothing → error (not a silent exit 0).
        let err = nonempty_pcm(&[], "USB Mic", 3.0).unwrap_err().to_string();
        assert!(err.contains("no audio captured"), "got: {err}");
        assert!(err.contains("USB Mic"), "should name the device: {err}");
    }

    #[test]
    fn nonempty_pcm_passes_through_captured_samples() {
        let events = [
            PipelineEvent::Frame(vec![0.1, 0.2]),
            PipelineEvent::Frame(vec![0.3]),
        ];
        assert_eq!(nonempty_pcm(&events, "", 1.0).unwrap(), vec![0.1, 0.2, 0.3]);
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
