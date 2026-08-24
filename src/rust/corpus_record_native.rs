//! Native Rust implementation of `whisper-dictate corpus-record <id>` — the
//! sole production surface for corpus recording.
//!
//! Runs entirely on the cpal capture path (`crate::audio::capture` →
//! `crate::audio::resampler`). The wire contract with the UI parser
//! ([`crate::ui::corpus_record`]) is preserved bit for bit:
//!
//!   * events are newline-delimited JSON (`corpus_record_start`,
//!     `corpus_record_progress`, `corpus_record_done`, `corpus_record_error`)
//!     — sent to stdout when driven from the CLI, buffered into a String
//!     when driven in-process from the UI background thread,
//!   * the WAV output is `<appdata>/benchmark/audio/<id>.wav` written as
//!     16 kHz mono 16-bit PCM (the established `wave.open` contract keeps
//!     pre-existing recordings interchangeable),
//!   * a bad corpus id / missing corpus / mic error is a single
//!     `corpus_record_error` line + exit 0 (never an unhandled panic).
//!
//! Feature-gated on `audio-capture` because it needs `cpal` + `rubato`;
//! a stock-build (no `audio-capture`) invocation of `whisper-dictate
//! corpus-record` returns a clear "rebuild with --features audio-capture"
//! error and exits non-zero from [`crate::corpus_record::handle_corpus_record`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;
use serde::Serialize;

use crate::audio::capture::{audio_chunk_channel, start_capture, AudioChunk, AudioChunkReceiver};
use crate::audio::resampler::FrameResampler;

/// The 16 kHz mono int16 target format for the golden-benchmark corpus WAVs.
/// Matches the established mono/16-bit/16 kHz corpus format so existing
/// recordings on user machines remain byte-format compatible.
const TARGET_SAMPLE_RATE: u32 = 16_000;

/// How often to emit a `corpus_record_progress` countdown line, in seconds.
/// Keeps a long read's `remaining_s` cadence stable across recording surfaces.
const PROGRESS_EVERY_S: f64 = 5.0;

/// The event envelopes the UI parses. Field names and null handling follow the
/// established payload contract (`ensure_ascii=false`). `#[serde(untagged)]`
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

/// Sink for one serialized JSON event line. The CLI sink writes to stdout
/// (flushed); the in-process UI sink appends to a String the background
/// thread hands to [`crate::ui::corpus_record::parse_corpus_record_result`]
/// once the run finishes. Using a `dyn FnMut` here means the same recording
/// pipeline drives both surfaces without a second code path.
type EventSink<'a> = dyn FnMut(String) + 'a;

/// Serialize one JSON event and hand the line to `sink`. `ensure_ascii=false`
/// equivalent: `serde_json` emits non-ASCII characters literally in a JSON
/// string (Danish reference text like `Hej` / `æøå` survives untouched).
fn emit_event(sink: &mut EventSink<'_>, event: &CorpusEvent<'_>) {
    // A serialisation failure here would be a bug in the event enum (all
    // fields are owned Rust primitives + borrowed &str), not a runtime
    // condition — but even in that impossible case we prefer a single log
    // line on stderr to a panic, so the record still exits 0.
    match serde_json::to_string(event) {
        Ok(line) => sink(line),
        Err(err) => {
            eprintln!("corpus-record: event serialize failed: {err}");
        }
    }
}

/// Emit a `corpus_record_error` line for `message` through `sink`.
///
/// Every failure — bad id, missing corpus, mic unavailable, disk-full on write
/// — is a single terminal error event and the run exits 0. That preserves the
/// contract the UI's terminal-event scanner
/// ([`crate::ui::corpus_record::parse_corpus_record_result`]) relies on: a
/// clean end-of-run marker rather than a subprocess/thread crash.
fn emit_error(sink: &mut EventSink<'_>, message: impl AsRef<str>) {
    emit_event(
        sink,
        &CorpusEvent::Error {
            event: "corpus_record_error",
            error: message.as_ref(),
        },
    );
}

/// Recording length (s) for `text`, cloned from [`crate::corpus_record::compute_record_seconds`].
///
/// Kept as a thin wrapper so this module and the pure-logic helper use the same
/// duration heuristic and the UI's `seconds` display stays consistent.
fn record_seconds_for(text: &str) -> f64 {
    crate::corpus_record::compute_record_seconds(text)
}

/// Resolve the corpus manifest, load it, and return the item matching `id`.
///
/// Uses the established app-root → appdata search order, so the recorder sees the
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
/// [`crate::ui::corpus::recorded_audio_path`] used by the picker.
pub(crate) fn output_wav_path(appdata: &Path, id: &str) -> PathBuf {
    appdata
        .join("benchmark")
        .join("audio")
        .join(format!("{id}.wav"))
}

/// Write the captured 16 kHz mono int16 PCM to `path` as a WAV.
///
/// The hound `WavSpec` matches the established mono/16-bit/16 kHz format byte
/// for byte, so an existing corpus WAV and a fresh recording are interchangeable
/// (same 44-byte PCM header,
/// same 16-bit little-endian samples). Creates parent dirs so the first-ever
/// recording works on a machine with no prior corpus data.
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
/// Matches the corpus level-reporting contract:
///
///   1. Normalize int16 by dividing by 32768.
///   2. `peak = max(|x|)`; `peak_dbfs = 20*log10(peak)` (or `-120.0` if zero).
///   3. `rms = sqrt(mean(x^2)) or 1e-9`; `rms_dbfs = 20*log10(max(rms, 1e-9))`.
///   4. Round both to 1 decimal.
///
/// Pure so the level report is unit-testable without opening a mic.
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
/// audit-able against the established `round(x, 1)` rule (banker's vs half-up rarely
/// matters at this precision, but keeping it explicit avoids surprises).
fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Convert a mono f32 sample (in `[-1.0, 1.0]`) to int16, clamping first.
/// Clamps before multiplying by 32767 to preserve the capture contract.
fn f32_to_i16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * f32::from(i16::MAX)) as i16
}

fn ensure_capture_queue_intact(chunk_rx: &AudioChunkReceiver) -> Result<(), String> {
    let dropped = chunk_rx.overflow_metric().count();
    if dropped == 0 {
        return Ok(());
    }
    Err(format!(
        "audio capture overflow dropped {dropped} chunk(s); recording was not saved, please retry"
    ))
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

    let (chunk_tx, chunk_rx) = audio_chunk_channel();
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
    ensure_capture_queue_intact(&chunk_rx)?;
    Ok(pcm)
}

/// Native `corpus-record <id>` CLI entry point — invoked from
/// [`crate::corpus_record::handle_corpus_record`] on `audio-capture` builds.
///
/// Prints one JSON event per line to stdout and always returns `Ok(())` (the
/// CLI exits 0) — every failure mode is a `corpus_record_error` line, so the
/// UI's terminal-event scanner always sees a clean end marker instead of a
/// process crash.
pub fn run_native(id: &str) -> anyhow::Result<()> {
    let mut sink = |line: String| {
        println!("{line}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    };
    run_native_with_sink(id, &mut sink)
}

/// In-process variant used by the UI: run the recorder on the current thread
/// and return the newline-joined JSON events the CLI would have printed.
///
/// The UI's background-task path spawns a thread that calls this and feeds the
/// returned String into
/// [`crate::ui::corpus_record::parse_corpus_record_result`] via the shared
/// [`crate::ui::BackgroundTaskResult`] envelope — the same parser the CLI-
/// subprocess path used, so the wire contract stays authoritative.
pub fn run_native_to_string(id: &str) -> String {
    let mut buf = String::new();
    let mut sink = |line: String| {
        buf.push_str(&line);
        buf.push('\n');
    };
    // Errors are already reported as `corpus_record_error` events on `sink`
    // (the pipeline never returns `Err`), so the caller only needs the
    // captured event stream.
    let _ = run_native_with_sink(id, &mut sink);
    buf
}

/// Shared implementation: drive one recording, feeding every JSON event line
/// through `sink`. `run_native` and `run_native_to_string` differ only in the
/// sink they install, so the recording logic (validation, corpus resolution,
/// capture, WAV write, dBFS report) has one home.
fn run_native_with_sink(id: &str, sink: &mut EventSink<'_>) -> anyhow::Result<()> {
    let id = id.trim();
    // id safety is re-checked by handle_corpus_record before we get here,
    // but the defence-in-depth check keeps this callable independently.
    if !crate::corpus_record::is_safe_corpus_id(id) {
        emit_error(sink, format!("unsafe corpus id: {id:?}"));
        return Ok(());
    }

    let app_root = crate::runtime::resource_app_root();
    let appdata = crate::config::platform_config_dir();

    let item = match resolve_item(id, &app_root, &appdata) {
        Ok(item) => item,
        Err(err) => {
            emit_error(sink, err);
            return Ok(());
        }
    };

    // Honour the user-configured max recording length even
    // when the corpus heuristic would ask for longer. The heuristic asks for
    // `record_seconds_for(text)` (2 s lead-in + `chars/12` clamped to 8..90);
    // if the user has capped their sessions below that (either persistently
    // via `max_record_s` or ephemerally via `VOICEPI_MAX_RECORD_S`) the
    // recorder must respect it, matching the cap used by normal dictation.
    // Resolve through the effective-setting pipeline so config > env >
    // default is applied consistently with normal dictation; then
    // clamp the corpus heuristic to that cap (a non-positive parsed value
    // disables the cap, same "0 = uncapped" contract as
    // `VOICEPI_MAX_RECORD_S`).
    let seconds = clamp_to_max_record(record_seconds_for(&item.text));
    emit_event(
        sink,
        &CorpusEvent::Start {
            event: "corpus_record_start",
            id: &item.id,
            text: &item.text,
            seconds: round1(seconds),
        },
    );

    // The configured mic, with env-var precedence baked in via
    // `worker_env_overrides()` (config > env > default), so a
    // `VOICEPI_AUDIO_DEVICE=Yeti whisper-dictate corpus-record …` recording
    // uses the shell-exported mic instead of silently falling through to the
    // OS default. Empty means "system default" (matches
    // `capture::start_capture`'s empty-selector semantics).
    let device = effective_audio_device();

    // Progress emission borrows `sink` for the duration of the capture call,
    // so we can't touch `sink` again until `capture_for` returns.
    let pcm = match capture_for(&device, seconds, |remaining| {
        emit_event(
            sink,
            &CorpusEvent::Progress {
                event: "corpus_record_progress",
                remaining_s: remaining,
            },
        );
    }) {
        Ok(pcm) => pcm,
        Err(err) => {
            emit_error(sink, err);
            return Ok(());
        }
    };

    if pcm.is_empty() {
        emit_error(sink, "no audio was captured (check the microphone)");
        return Ok(());
    }

    let out_path = output_wav_path(&appdata, &item.id);
    if let Err(err) = write_wav_int16(&out_path, &pcm) {
        emit_error(sink, err);
        return Ok(());
    }

    let (peak_dbfs, rms_dbfs) = peak_rms_dbfs(&pcm);
    let seconds_recorded = round1(pcm.len() as f64 / f64::from(TARGET_SAMPLE_RATE));
    emit_event(
        sink,
        &CorpusEvent::Done {
            event: "corpus_record_done",
            id: &item.id,
            path: &out_path.to_string_lossy(),
            seconds_recorded,
            peak_dbfs,
            rms_dbfs,
        },
    );
    Ok(())
}

/// Resolve the microphone the recorder should open, honouring
/// `VOICEPI_AUDIO_DEVICE` when the on-disk config leaves `audio_device`
/// blank. Consults `worker_env_overrides()` so precedence matches the rest
/// of the runtime (config > env > default) instead of reading only the raw
/// settings field the way `crate::config::load_settings().audio_device`
/// did.
///
/// Empty string means "system default" and is honoured by
/// `crate::audio::capture::start_capture`.
pub(crate) fn effective_audio_device() -> String {
    let overrides = crate::config::worker_env_overrides();
    for (key, value) in &overrides {
        if key == crate::runtime::audio_spawn::AUDIO_DEVICE_ENV {
            return value.trim().to_owned();
        }
    }
    String::new()
}

/// The `max_record_s` env-var name used by native recording.
const MAX_RECORD_ENV: &str = "VOICEPI_MAX_RECORD_S";

/// Default cap in seconds when `VOICEPI_MAX_RECORD_S` is unset OR
/// unparseable.
const DEFAULT_MAX_RECORD_S: f64 = 120.0;

/// Clamp the corpus heuristic's requested recording duration to the user's
/// effective `max_record_s` cap. Reads through the same effective-setting
/// pipeline normal dictation uses, so a persistent config value (`config.json`)
/// and a shell-exported `VOICEPI_MAX_RECORD_S` are honoured with the same
/// config > env > default precedence. A non-positive parsed cap disables the
/// clamp (0 = uncapped).
pub(crate) fn clamp_to_max_record(seconds: f64) -> f64 {
    let overrides = crate::config::worker_env_overrides();
    let raw = overrides
        .iter()
        .find(|(k, _)| k == MAX_RECORD_ENV)
        .map(|(_, v)| v.as_str());
    clamp_to_max_record_with(seconds, raw)
}

/// Testable core of [`clamp_to_max_record`]: takes the already-resolved
/// `max_record_s` string (as `worker_env_overrides` would emit) so the
/// parse + clamp behaviour can be pinned without a config file or a
/// process env toggle. Unparseable / missing -> default (120 s); a
/// `"0"` (or any non-positive / non-finite parsed value) disables the cap.
pub(crate) fn clamp_to_max_record_with(seconds: f64, raw: Option<&str>) -> f64 {
    let cap = match raw {
        Some(value) => match value.trim().parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 => Some(v),
            Ok(_) => None,
            Err(_) => Some(DEFAULT_MAX_RECORD_S),
        },
        None => Some(DEFAULT_MAX_RECORD_S),
    };
    match cap {
        Some(cap) => seconds.min(cap),
        None => seconds,
    }
}

// Tests live in a sibling file so this module stays under the 500-line
// modularity limit. The `#[path]` attribute keeps them logically nested so
// they still see the private items (`f32_to_i16`, `round1`, `resolve_item`,
// `write_wav_int16`, `CorpusEvent`, `TARGET_SAMPLE_RATE`) via `use super::*`.
#[cfg(test)]
#[path = "corpus_record_native_tests.rs"]
mod tests;
