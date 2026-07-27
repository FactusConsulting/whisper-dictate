//! Native Rust implementation of `whisper-dictate corpus-record <id>` — step 1
//! of retiring the Python `vp_corpus_record.py` worker (Wave 6 of #348,
//! same pattern as PR #600 for `devices test`).
//!
//! Runs entirely on the cpal capture path (`crate::audio::capture` →
//! `crate::audio::resampler`) so a stock Rust build no longer needs the
//! Python `vp_capture` machinery to record golden benchmark audio. The
//! wire contract with the UI is preserved bit for bit:
//!
//!   * stdout is newline-delimited JSON events (`corpus_record_start`,
//!     `corpus_record_progress`, `corpus_record_done`, `corpus_record_error`),
//!   * the WAV output is `<appdata>/benchmark/audio/<id>.wav` written as
//!     16 kHz mono 16-bit PCM (matches Python's `wave.open`/`setsampwidth(2)`),
//!   * a bad corpus id / missing corpus / mic error is a single
//!     `corpus_record_error` line + exit 0 (never an unhandled panic).
//!
//! The UI parser in [`crate::ui::corpus_record`] therefore keeps working
//! unchanged; the Python subprocess is simply replaced by an in-process
//! cpal recorder that emits the same JSON envelopes.
//!
//! Feature-gated on `audio-capture` because it needs `cpal` + `rubato`;
//! the stock-build fallback lives in [`crate::corpus_record`] and shells
//! to the Python worker exactly like the pre-migration path.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::audio::capture::{start_capture, AudioChunk};
use crate::audio::resampler::FrameResampler;

/// The 16 kHz mono int16 target format for the golden-benchmark corpus WAVs.
/// Matches `vp_corpus_record._write_wav` (`setnchannels(1)`, `setsampwidth(2)`,
/// `setframerate(vp_capture.SR)`) so existing corpus recordings on user
/// machines remain byte-format compatible.
const TARGET_SAMPLE_RATE: u32 = 16_000;

/// How often to emit a `corpus_record_progress` countdown line, in seconds.
/// Matches `_PROGRESS_EVERY_S` in the Python worker so a long read shows the
/// same "remaining_s" cadence regardless of which recorder is in use.
const PROGRESS_EVERY_S: f64 = 5.0;

/// The event envelopes the UI parses. Field names + null handling match the
/// Python `_print_event` payloads 1:1 (`ensure_ascii=False`). `#[serde(untagged)]`
/// keeps the wire shape flat — one top-level `"event"` field plus per-variant
/// fields — the same shape [`crate::ui::corpus_record::parse_corpus_record_result`]
/// expects.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum CorpusEvent<'a> {
    Start {
        event: &'static str,
        id: &'a str,
        text: &'a str,
        seconds: f64,
    },
    Progress {
        event: &'static str,
        remaining_s: i64,
    },
    Done {
        event: &'static str,
        id: &'a str,
        path: &'a str,
        seconds_recorded: f64,
        peak_dbfs: f64,
        rms_dbfs: f64,
    },
    Error {
        event: &'static str,
        error: &'a str,
    },
}

/// Print one JSON event line to stdout, flushed. `ensure_ascii=false`
/// equivalent: `serde_json` emits non-ASCII characters literally in a JSON
/// string (Danish reference text like `Hej` / `æøå` survives untouched),
/// matching `vp_corpus_record._print_event` which passes `ensure_ascii=False`.
fn emit_event(event: &CorpusEvent<'_>) {
    // A serialisation failure here would be a bug in the event enum (all
    // fields are owned Rust primitives + borrowed &str), not a runtime
    // condition — but even in that impossible case we prefer a single log
    // line on stderr to a panic, so the record still exits 0.
    match serde_json::to_string(event) {
        Ok(line) => {
            println!("{line}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        Err(err) => {
            eprintln!("corpus-record: event serialize failed: {err}");
        }
    }
}

/// Emit a `corpus_record_error` line for `message` and return `Ok(())`.
///
/// The Python worker treats every failure — bad id, missing corpus, mic
/// unavailable, disk-full on write — as a single terminal error event and
/// exits 0. We keep exactly that contract so the UI's terminal-event scanner
/// ([`crate::ui::corpus_record::parse_corpus_record_result`]) always sees a
/// clean end-of-run marker rather than a subprocess crash.
fn emit_error(message: impl AsRef<str>) {
    emit_event(&CorpusEvent::Error {
        event: "corpus_record_error",
        error: message.as_ref(),
    });
}

/// Recording length (s) for `text`, cloned from [`crate::corpus_record::compute_record_seconds`].
///
/// Kept as a thin wrapper so this module and the pure-logic port are pinned to
/// the same duration heuristic — regressing one and not the other would break
/// parity with the Python side and the UI's `seconds` display.
fn record_seconds_for(text: &str) -> f64 {
    crate::corpus_record::compute_record_seconds(text)
}

/// Resolve the corpus manifest, load it, and return the item matching `id`.
///
/// Uses the SAME app-root → appdata search order the Python worker did
/// (`vp_benchmark_paths.resolve_corpus_manifest`), so the recorder sees the
/// same corpus the "Run benchmark" button would. Missing corpus / unknown id
/// / parse error are all translated into a short error message that becomes a
/// `corpus_record_error` event upstream.
fn resolve_item(
    id: &str,
    app_root: &Path,
    appdata: &Path,
) -> Result<crate::corpus::CorpusItem, String> {
    let manifest = crate::corpus::resolve_corpus_manifest(Some(app_root), None, Some(appdata))
        .ok_or_else(|| "no benchmark corpus found".to_owned())?;
    if !manifest.exists() {
        return Err(format!(
            "no benchmark corpus found (looked at {})",
            manifest.display()
        ));
    }
    let items = crate::corpus::load_corpus(&manifest).map_err(|e| e.to_string())?;
    items
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("unknown corpus id: {id}"))
}

/// Where the recorded WAV goes: `<appdata>/benchmark/audio/<id>.wav`.
///
/// Pure so it is unit-testable without touching the real appdata dir. Mirrors
/// the Python `appdata_audio_dir(appdata) / f"{id}.wav"` and the Rust
/// [`crate::ui::corpus::recorded_audio_path`] the picker already uses.
pub(crate) fn output_wav_path(appdata: &Path, id: &str) -> PathBuf {
    appdata
        .join("benchmark")
        .join("audio")
        .join(format!("{id}.wav"))
}

/// Write the captured 16 kHz mono int16 PCM to `path` as a WAV.
///
/// The hound `WavSpec` here matches Python's `wave.setnchannels(1)` /
/// `setsampwidth(2)` / `setframerate(16000)` byte for byte, so an existing
/// corpus WAV recorded by the Python worker and a fresh one recorded by
/// this native path are interchangeable (same 44-byte PCM header,
/// same 16-bit little-endian samples). Creates parent dirs so the first-ever
/// recording works on a machine that never ran the Python worker.
fn write_wav_int16(path: &Path, pcm: &[i16]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create audio dir: {e}"))?;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| format!("wav open: {e}"))?;
    for &sample in pcm {
        writer
            .write_sample(sample)
            .map_err(|e| format!("wav write: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("wav finalize: {e}"))?;
    Ok(())
}

/// Compute `(peak_dbfs, rms_dbfs)` for an int16 buffer, rounded to 1 decimal.
///
/// Matches Python's `vp_corpus_record._peak_rms_dbfs`:
///
///   1. Normalize int16 by dividing by 32768.
///   2. `peak = max(|x|)`; `peak_dbfs = 20*log10(peak)` (or `-120.0` if zero).
///   3. `rms = sqrt(mean(x^2)) or 1e-9`; `rms_dbfs = 20*log10(max(rms, 1e-9))`.
///   4. Round both to 1 decimal.
///
/// Pure so the level report is unit-testable against Python's shape without
/// opening a mic.
pub(crate) fn peak_rms_dbfs(pcm: &[i16]) -> (f64, f64) {
    if pcm.is_empty() {
        return (-120.0, round1(20.0 * (1e-9_f64).log10()));
    }
    let mut peak: f64 = 0.0;
    let mut sum_sq: f64 = 0.0;
    for &s in pcm {
        let norm = f64::from(s) / 32768.0;
        let abs = norm.abs();
        if abs > peak {
            peak = abs;
        }
        sum_sq += norm * norm;
    }
    let rms = (sum_sq / pcm.len() as f64).sqrt().max(1e-9);
    let peak_dbfs = if peak > 0.0 {
        20.0 * peak.log10()
    } else {
        -120.0
    };
    let rms_dbfs = 20.0 * rms.log10();
    (round1(peak_dbfs), round1(rms_dbfs))
}

/// Round to one decimal place. Extracted so the rounding rule is trivially
/// audit-able against Python's `round(x, 1)` (banker's vs half-up rarely
/// matters at this precision, but keeping it explicit avoids surprises).
fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Convert a mono f32 sample (in `[-1.0, 1.0]`) to int16, clamping first.
/// Matches Python's `_capture_frame_to_int16` (clamp → multiply by 32767).
fn f32_to_i16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * f32::from(i16::MAX)) as i16
}

/// The mic-open path and the recording loop, isolated from the UI-facing
/// event stream so it is unit-testable in principle. Returns the captured
/// 16 kHz mono int16 PCM plus the negotiated native sample rate (surfaced in
/// logs on error) or a short error string.
///
/// `on_progress` is called every `PROGRESS_EVERY_S` seconds with the
/// remaining seconds, so the caller can emit the `corpus_record_progress`
/// countdown line without this function knowing about JSON.
fn capture_for(
    device: &str,
    seconds: f64,
    mut on_progress: impl FnMut(i64),
) -> Result<Vec<i16>, String> {
    // v1.20.6 PipeWire quantum mitigation, same as `audio::self_test` /
    // `dictate::mic`: apply BEFORE opening cpal so a Linux DMIC / PipeWire
    // host doesn't quietly negotiate a 4096-sample quantum that starves the
    // callback. No-op on non-Linux / when the operator set PIPEWIRE_QUANTUM.
    let _ = crate::audio::pipewire::configure_pipewire_env();

    let (chunk_tx, chunk_rx) = mpsc::channel::<AudioChunk>();
    let mut capture =
        start_capture(device, chunk_tx).map_err(|e| format!("open capture device: {e:#}"))?;
    let native_rate = capture.sample_rate();
    let mut resampler = FrameResampler::new(native_rate as usize)
        .map_err(|e| format!("construct 16k resampler: {e}"))?;

    let mut pcm: Vec<i16> = Vec::with_capacity((seconds * TARGET_SAMPLE_RATE as f64) as usize);
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(seconds);
    let mut next_progress = start + Duration::from_secs_f64(PROGRESS_EVERY_S);

    let absorb_frame = |frame: &[f32], pcm: &mut Vec<i16>| {
        for &s in frame {
            pcm.push(f32_to_i16(s));
        }
    };

    let mut fatal_error: Option<String> = None;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        // Also wake up periodically so we can emit progress even if the mic
        // callback is slow. Bounded by the shorter of remaining time OR the
        // next-progress deadline so we never oversleep either.
        let wake_by = if now < next_progress {
            (next_progress - now).min(remaining)
        } else {
            remaining
        };
        match chunk_rx.recv_timeout(wake_by) {
            Ok(AudioChunk::Samples(samples)) => {
                resampler.push(&samples, |frame| absorb_frame(frame, &mut pcm));
            }
            Ok(AudioChunk::EndOfStream) => break,
            Ok(AudioChunk::Error(msg)) => {
                fatal_error = Some(msg);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                // Fall through to the progress check below.
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now2 = Instant::now();
        if now2 >= next_progress && now2 < deadline {
            let remaining_s = (deadline - now2).as_secs_f64().round() as i64;
            on_progress(remaining_s);
            next_progress = now2 + Duration::from_secs_f64(PROGRESS_EVERY_S);
        }
    }

    // Stop the stream so the mic is released promptly, then drain any tail
    // chunks that landed on the channel after `deadline` (WASAPI can queue a
    // callback right around the deadline boundary; keeping those samples
    // avoids clipping the last 20-40 ms of speech).
    capture.stop();
    while let Ok(chunk) = chunk_rx.try_recv() {
        match chunk {
            AudioChunk::Samples(samples) => {
                resampler.push(&samples, |frame| absorb_frame(frame, &mut pcm));
            }
            AudioChunk::EndOfStream => {}
            AudioChunk::Error(msg) => {
                if fatal_error.is_none() {
                    fatal_error = Some(msg);
                }
            }
        }
    }
    // Flush the resampler's trailing partial frame so we don't lose ~30 ms
    // of trailing audio (matches the `resampler::FrameResampler::finish`
    // contract the raw pump exercises).
    resampler.finish(|frame| absorb_frame(frame, &mut pcm));

    if let Some(msg) = fatal_error {
        return Err(msg);
    }
    Ok(pcm)
}

/// Native `corpus-record <id>` entry point.
///
/// Always returns `Ok(())` (the CLI exits 0) — every failure mode is a
/// `corpus_record_error` JSON line on stdout, matching the Python worker's
/// "an error is a normal, reportable outcome" contract so the UI's terminal-
/// event scanner sees a clean end marker instead of a subprocess crash.
///
/// This is invoked from [`crate::corpus_record::handle_corpus_record`] on
/// `audio-capture` builds; stock builds still shell out to the Python
/// worker until the step-2 PR retires it.
pub fn run_native(id: &str) -> anyhow::Result<()> {
    let id = id.trim();
    // id safety is re-checked by handle_corpus_record before we get here,
    // but the defence-in-depth check keeps this callable independently.
    if !crate::corpus_record::is_safe_corpus_id(id) {
        emit_error(format!("unsafe corpus id: {id:?}"));
        return Ok(());
    }

    let app_root = crate::runtime::resource_app_root();
    let appdata = crate::config::platform_config_dir();

    let item = match resolve_item(id, &app_root, &appdata) {
        Ok(item) => item,
        Err(err) => {
            emit_error(err);
            return Ok(());
        }
    };

    let seconds = record_seconds_for(&item.text);
    emit_event(&CorpusEvent::Start {
        event: "corpus_record_start",
        id: &item.id,
        text: &item.text,
        seconds: round1(seconds),
    });

    // The configured mic. Empty means "system default" (matches
    // `capture::start_capture`'s empty-selector semantics and Python's
    // `sd.default.device[0] is None` fallback).
    let device = crate::config::load_settings()
        .map(|s| s.audio_device)
        .unwrap_or_default();

    let pcm = match capture_for(&device, seconds, |remaining| {
        emit_event(&CorpusEvent::Progress {
            event: "corpus_record_progress",
            remaining_s: remaining,
        });
    }) {
        Ok(pcm) => pcm,
        Err(err) => {
            emit_error(err);
            return Ok(());
        }
    };

    if pcm.is_empty() {
        emit_error("no audio was captured (check the microphone)");
        return Ok(());
    }

    let out_path = output_wav_path(&appdata, &item.id);
    if let Err(err) = write_wav_int16(&out_path, &pcm) {
        emit_error(err);
        return Ok(());
    }

    let (peak_dbfs, rms_dbfs) = peak_rms_dbfs(&pcm);
    let seconds_recorded = round1(pcm.len() as f64 / f64::from(TARGET_SAMPLE_RATE));
    emit_event(&CorpusEvent::Done {
        event: "corpus_record_done",
        id: &item.id,
        path: &out_path.to_string_lossy(),
        seconds_recorded,
        peak_dbfs,
        rms_dbfs,
    });
    Ok(())
}

// Tests live in a sibling file so this module stays under the 500-line
// modularity limit. The `#[path]` attribute keeps them logically nested so
// they still see the private items (`f32_to_i16`, `round1`, `resolve_item`,
// `write_wav_int16`, `CorpusEvent`, `TARGET_SAMPLE_RATE`) via `use super::*`.
#[cfg(test)]
#[path = "corpus_record_native_tests.rs"]
mod tests;
