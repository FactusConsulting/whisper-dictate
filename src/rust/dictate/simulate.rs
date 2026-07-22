//! `whisper-dictate simulate-session` — offline, WAV-driven end-to-end drive
//! of the in-process Rust [`DictateSession`], for CLI integration testing of
//! the **new** (Rust) engine alongside the **old** (Python) `simulate-ptt`.
//!
//! The Python worker has `python -m whisper_dictate.vp_simulate_ptt --wav …`
//! which runs its full pipeline (transcribe → post-process → format →
//! inject-preview) on a fixture WAV. The Rust engine had no equivalent: its
//! only CLI entry (`dictate-run`) is live/hotkey-driven, and `simulate-ptt`
//! forwards to Python. This verb closes that gap so BOTH engines are
//! exercised from the CLI while Python still exists.
//!
//! It drives the REAL `DictateSession` through the same
//! `start → push_frame → stop_and_transcribe` API the live supervisor uses,
//! over the **cloud** STT backend (Groq/OpenAI) so it needs no local model,
//! GPU, mic, or cargo feature — the whole path is stock. Injection is
//! preview-only (captured, never sent to the OS). Post-processing and
//! format-commands are applied from the same `VOICEPI_*` env the worker
//! command exports, so the pass mirrors production ordering.

use std::cell::RefCell;
use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::dictate::backends::cloud_transcribe::cloud_backend_local_only_checked;
use crate::dictate::{
    CloudTranscribeConfig, DictateSession, InjectBackend, InjectError, SessionConfig,
    TranscribeBackend, UtteranceOutcome,
};
use crate::whisper::decode_wav_16k_mono;

/// Env key for the spoken formatting-command set (`off`/`en`/`da`/`both`),
/// mirroring `VOICEPI_FORMAT_COMMANDS` in the real session config.
const FORMAT_COMMANDS_ENV: &str = "VOICEPI_FORMAT_COMMANDS";

/// Preview-only inject backend: records each injected string instead of
/// touching the OS keyboard. Interior mutability (single-threaded CLI) so
/// the trait's `&self` signature is satisfied without an OS backend.
#[derive(Default)]
pub struct CaptureInject {
    injected: RefCell<Vec<String>>,
}

impl CaptureInject {
    /// The texts injected so far, in order.
    pub fn injected(&self) -> Vec<String> {
        self.injected.borrow().clone()
    }
}

impl InjectBackend for CaptureInject {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        self.injected.borrow_mut().push(text.to_owned());
        Ok(())
    }
}

/// Frame size used to feed the buffered PCM into the session, exercising the
/// multi-`push_frame` buffering path rather than a single giant push. The
/// session concatenates frames, so the exact value only affects how many
/// `push_frame` calls happen (0.1 s at 16 kHz).
const DRIVE_FRAME: usize = 1_600;

/// Build the [`SessionConfig`] for the simulate drive from the process env.
/// Only the format-command set is env-sourced here; the min-record floor
/// keeps its default (the fixtures are comfortably above it).
fn simulate_session_config() -> SessionConfig {
    let format_command_set = std::env::var(FORMAT_COMMANDS_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    SessionConfig {
        format_command_set,
        ..SessionConfig::default()
    }
}

/// Drive a session end-to-end over `pcm`: `start`, push in [`DRIVE_FRAME`]
/// chunks, then `stop_and_transcribe`. Worker events are written to
/// `writer`. Generic over the backends so it is unit-testable with stubs
/// (the CLI handler binds the cloud + capture backends).
pub fn drive_session_over_pcm<T, I, W>(
    session: &mut DictateSession<T, I>,
    pcm: &[f32],
    writer: &mut W,
) -> Result<UtteranceOutcome>
where
    T: TranscribeBackend,
    I: InjectBackend,
    W: Write,
{
    session
        .start(writer)
        .map_err(|e| anyhow!("session start failed: {e}"))?;
    // Empty `pcm` yields no chunks, so no frames are pushed and `stop`
    // resolves to `NoAudio` — no special-case needed.
    for chunk in pcm.chunks(DRIVE_FRAME.max(1)) {
        session.push_frame(chunk);
    }
    session
        .stop_and_transcribe(writer)
        .map_err(|e| anyhow!("session stop/transcribe failed: {e}"))
}

/// CLI entry: decode `wav_path`, drive the real session over the cloud STT
/// backend, and report the result. With `json`, the session's worker-event
/// stream is written to stdout (one JSON object per line); otherwise the
/// final injected transcript is printed (and a diagnostic line on any
/// no-text outcome).
/// Resolve the cloud transcribe backend for the drive: require a configured
/// backend (non-empty key + model) with an actionable error otherwise, then
/// enforce the local-only privacy lock. `local_only` is passed in (the
/// handler supplies [`crate::whisper::model_manager::is_local_only`]) so the
/// resolution is unit-testable without touching process env / network.
fn resolve_cloud_transcribe(
    config: CloudTranscribeConfig,
    local_only: bool,
) -> Result<crate::dictate::CloudTranscribeBackend> {
    if config.api_key.trim().is_empty() || config.model.trim().is_empty() {
        return Err(anyhow!(
            "simulate-session drives the Rust DictateSession over the cloud STT \
             backend; set VOICEPI_STT_MODEL and an API key \
             (VOICEPI_STT_API_KEY / GROQ_API_KEY / OPENAI_API_KEY, per \
             VOICEPI_STT_BASE_URL)."
        ));
    }
    cloud_backend_local_only_checked(local_only, config).map_err(|e| anyhow!(e))
}

/// Drive `session` over `pcm` for `repeat` consecutive press → release
/// cycles, reusing the SAME session each time. Returns one outcome per
/// cycle, in order. `repeat == 0` is treated as a single cycle so the CLI
/// never silently does nothing.
///
/// This is the session-reuse path: a `DictateSession` that only armed once
/// (the "PTT works the first time then gets stuck" bug the Rust flip hit
/// before 1.21.0) would fail the 2nd `start()` with `AlreadyActive`, which
/// `drive_session_over_pcm` surfaces as an `Err`. Driving ≥2 cycles here —
/// and asserting each transcribes in the integration smoke test — catches
/// that regression against the REAL backend.
pub fn drive_session_cycles<T, I, W>(
    session: &mut DictateSession<T, I>,
    pcm: &[f32],
    writer: &mut W,
    repeat: u32,
) -> Result<Vec<UtteranceOutcome>>
where
    T: TranscribeBackend,
    I: InjectBackend,
    W: Write,
{
    let cycles = repeat.max(1);
    let mut outcomes = Vec::with_capacity(cycles as usize);
    for cycle in 1..=cycles {
        let outcome = drive_session_over_pcm(session, pcm, writer)
            .map_err(|e| anyhow!("cycle {cycle}/{cycles}: {e}"))?;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

pub fn handle_simulate_session(wav_path: &str, json: bool, repeat: u32) -> Result<()> {
    // Dictionary support, mirroring the Python worker + the in-process session:
    // term-based prompt biasing folds into the cloud STT prompt, and the
    // replacement table is attached to the session (applied to the transcript
    // before post-process/format/inject). Loaded from the same
    // `VOICEPI_DICTIONARY*` env + config the `dictionary-runtime` RPC reads;
    // disabled / empty is a no-op.
    let dictionary = crate::dictionary::load_session_dictionary();

    // This verb drives the cloud path (stock, no local model needed). Fold the
    // dictionary terms into the endpoint's initial prompt.
    let mut cloud_config = CloudTranscribeConfig::from_env();
    let base_prompt = cloud_config.prompt.take();
    cloud_config.prompt = dictionary.initial_prompt(base_prompt.as_deref());
    let transcribe =
        resolve_cloud_transcribe(cloud_config, crate::whisper::model_manager::is_local_only())?;

    let pcm = decode_wav_16k_mono(Path::new(wav_path))
        .map_err(|e| anyhow!("decode {wav_path}: {e:#}"))?;

    let inject = CaptureInject::default();
    let mut session = DictateSession::new(transcribe, inject, simulate_session_config());
    if dictionary.has_replacements() {
        session = session.with_dictionary(dictionary.dictionary);
    }
    if let Some(post) = crate::postprocess::SessionPostProcess::from_env() {
        session = session.with_post_process(Box::new(post));
    }

    let outcomes = if json {
        // The session gates its worker-event lines behind VOICEPI_WORKER_EVENTS
        // (so an ungated CLI drive never leaks them). `--json` explicitly asks
        // for that stream, so enable the gate for this process if it isn't
        // already on.
        if !crate::dictate::env_gates::is_truthy(
            std::env::var("VOICEPI_WORKER_EVENTS").ok().as_deref(),
        ) {
            std::env::set_var("VOICEPI_WORKER_EVENTS", "1");
        }
        // Buffer, then strip the session's `[worker-event] ` line prefix so
        // stdout is valid JSONL (one JSON object per line) as `--json`
        // promises, rather than the wire-format `[worker-event] {…}` lines.
        // All cycles stream into the same buffer, so the JSONL covers every
        // press → release in order.
        let mut buf = Vec::new();
        let outcomes = drive_session_cycles(&mut session, &pcm, &mut buf, repeat)?;
        let jsonl = to_clean_jsonl(&String::from_utf8_lossy(&buf));
        if !jsonl.is_empty() {
            println!("{jsonl}");
        }
        outcomes
    } else {
        let mut sink = std::io::sink();
        drive_session_cycles(&mut session, &pcm, &mut sink, repeat)?
    };

    if !json {
        for (i, outcome) in outcomes.iter().enumerate() {
            match outcome {
                UtteranceOutcome::Injected { text, .. } => println!("{text}"),
                other => eprintln!(
                    "[simulate-session] cycle {}: no injection ({other:?})",
                    i + 1
                ),
            }
        }
    }
    Ok(())
}

/// Convert the session's buffered worker-event stream into clean JSONL:
/// drop the `[worker-event] ` line prefix the wire emitter adds (see
/// [`crate::dictate::events::WORKER_EVENT_PREFIX`]) and skip blank lines, so
/// `--json` output is one valid JSON object per line. Lines without the
/// prefix pass through unchanged.
fn to_clean_jsonl(raw: &str) -> String {
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.strip_prefix(crate::dictate::events::WORKER_EVENT_PREFIX)
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "simulate_tests.rs"]
mod tests;
