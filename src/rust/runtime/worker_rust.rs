//! Wave 5 PR 6+7 of #348: the `whisper-dictate worker-rust` CLI entry
//! point — a long-running, in-process Rust dictation worker that
//! REPLACES the Python `vp_dictate.py` / `runtime.py` orchestrator on
//! any build compiled with the full feature set
//! (`whisper-rs-local,rust-injection,audio-in-rust,rust-hotkeys`).
//!
//! The subprocess driven by this entry point owns the full dictation
//! lifecycle: the hotkey coordinator, the production
//! [`crate::dictate::DictateSession`] (via
//! [`super::rust_session_sink::build_production_sink`]), the rdev OS
//! listener (skipped on `--stdin-only`), a stdin command parser
//! (`press` / `release` / `cancel` / `quit`), and a foreground event-
//! pump that forwards [`RuntimeEvent`]s to stderr/stdout in the same
//! wire format the supervisor's `parse_worker_event` consumer already
//! ingests.
//!
//! # PR 7 default: Rust worker; escape hatch: Python
//!
//! * `VOICEPI_DICTATE_BACKEND` unset + all four features ->
//!   supervisor spawns THIS worker (production default; the Rust
//!   worker owns dictation end-to-end).
//! * `VOICEPI_DICTATE_BACKEND=python-legacy` (any casing/whitespace)
//!   with the Python bundle still installed triggers an emergency
//!   rollback: supervisor spawns Python
//!   (`python -m whisper_dictate.runtime`) exactly like the pre-PR-7
//!   default. Provided so a real-world regression during the Wave-7
//!   -> Wave-8 burn-in can be un-stuck without a rollback build. Wave 8
//!   deletes the Python bundle and drops this escape hatch (issue #348).
//! * Any feature in the four-feature set missing -> supervisor stays
//!   on Python regardless of the env var (this subcommand exits
//!   non-zero with a clear "feature not compiled in" message if
//!   invoked directly). This branch is compiled-out on stock CI
//!   builds so PR 6's pre-flip semantics still hold for dev/test.
//!
//! Historical values (`VOICEPI_DICTATE_BACKEND=rust`,
//! `=rust-session`) are still recognised by
//! [`super::rust_session_sink::dictate_backend_rust_session_requested`]
//! for the supervisor-side session-sink hotkey routing on the Python
//! fallback path; on the new-default delegate path those knobs are
//! moot because the subprocess owns everything.
//!
//! # Test mode (`--stdin-only`)
//!
//! Stock-feature CI cannot install rdev (no display / permission), so
//! the hidden `--stdin-only` flag skips the rdev install and drives
//! the coordinator from stdin commands. The session sink is still
//! the real one (or the PR 4 stub on stock builds) so the
//! coordinator -> sink -> session -> emitter chain is exercised end-
//! to-end. EOF on stdin is treated as `quit` so the supervisor can
//! shut the worker down by closing its pipe end (the portable
//! shutdown mechanism on Windows where SIGTERM isn't reliable).
//!
//! Integration test lives at
//! `src/rust/tests/worker_rust_subprocess.rs` (Cargo integration
//! test) because it spawns the binary -- `env!("CARGO_BIN_EXE_*")`
//! is only defined for integration tests.

use std::io::{self, BufRead, BufReader, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use anyhow::{anyhow, Result};

use crate::dictate::events as dictate_events;
use crate::hotkey::coordinator::{
    spawn as spawn_coordinator, CoordinatorEvent, CoordinatorHandle, Mode, Options,
};
use crate::runtime::{
    extract_hotkey_key_names, parse_toggle_value, rust_session_sink, RuntimeEvent, WorkerCommand,
};

#[cfg(test)]
#[path = "worker_rust_tests.rs"]
mod tests;

/// True when this build was compiled with every feature the production
/// rust-session worker needs. The supervisor uses this -- together with
/// [`rust_session_sink::dictate_backend_rust_session_requested`] -- to
/// decide whether to delegate to the worker-rust subprocess instead of
/// the Python orchestrator. A `--stdin-only` test build bypasses the
/// audio pump + rdev install, but the production sink helper still
/// expects all four features to be on.
///
/// All four flags are required:
///
/// * `whisper-rs-local` -- the real Whisper backend.
/// * `rust-injection` -- the real OS injector (enigo).
/// * `audio-in-rust` -- the cpal audio pump.
/// * `rust-hotkeys` -- the rdev OS hotkey listener.
///
/// Without any one of them, the production rust-session path cannot
/// drive a real dictation; the supervisor stays on Python.
pub const fn all_required_features_enabled() -> bool {
    cfg!(all(
        feature = "whisper-rs-local",
        feature = "rust-injection",
        feature = "audio-in-rust",
        feature = "rust-hotkeys",
    ))
}

/// Whether the supervisor should delegate the dictation lifecycle to
/// the worker-rust subprocess. True iff:
///
/// * the binary was built with [`all_required_features_enabled`], AND
/// * the effective `VOICEPI_STT_BACKEND` names a backend the rust
///   worker knows how to build
///   ([`unsupported_worker_rust_settings_reason`] returns `None`).
///
/// **Wave 8 Part 2 (Codex #453 P2)**: the `python-legacy` escape hatch
/// no longer participates in this gate. Wave 8 removed the Python
/// bundle, so a stale `VOICEPI_DICTATE_BACKEND=python-legacy` in an
/// upgraded user's env would otherwise block the ONLY remaining
/// worker. The value is now silently ignored; a one-line stderr hint
/// (see [`warn_stale_python_legacy`]) surfaces at supervisor startup
/// so users know to unset it.
///
/// # The `env` parameter (Codex #441 P2 review round 3)
///
/// `env` is the effective command env the supervisor will pass to the
/// worker-rust subprocess (`WorkerCommand::env`). AppSettings values
/// (`stt_backend`, `local_only`, ...) materialise into that vec via
/// [`crate::config::worker_env_overrides`] BEFORE they appear in
/// `std::env`, so the gate MUST consult the vec first -- otherwise
/// process-env-only wiring for the same key would miss the config-derived
/// override.
///
/// The lookup helper ([`resolved_env`]) falls back to `std::env::var`
/// when the key is absent from the vec so process-env-only wiring
/// (`VOICEPI_WHISPER_MODEL_PATH`, escape-hatch overrides in an
/// interactive shell) still counts -- mirrors what the child actually
/// sees once it inherits the parent env on top of the overrides.
///
/// Pure helper so the gate is unit-testable. The supervisor's
/// `RuntimeSupervisor::start` consults this to swap its spawned
/// command for the worker-rust subcommand AND to skip its own
/// in-process Rust hotkey install (the subprocess installs its own).
pub fn should_delegate_to_worker_rust(env: &[(String, String)]) -> bool {
    all_required_features_enabled() && unsupported_worker_rust_settings_reason(env).is_none()
}

/// Emit a one-time stderr hint when a stale
/// `VOICEPI_DICTATE_BACKEND=python-legacy` is present in `env`. Wave 8
/// Part 2 dropped the escape hatch (Codex #453 P2), but users who
/// upgraded from a Wave-7 install may still carry the value in their
/// shell profile / config. Idempotent: called once by
/// `RuntimeSupervisor::start`; guarded by an `AtomicBool` so a
/// restart cycle does not spam the log.
pub fn warn_stale_python_legacy_if_set(env: &[(String, String)]) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !rust_session_sink::dictate_backend_python_legacy_requested_from(env) {
        return;
    }
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "[runtime] VOICEPI_DICTATE_BACKEND=python-legacy is set but has no effect \
         in v1.20 -- the Python worker was removed. Unset the env var to silence \
         this hint."
    );
}

/// Env var carrying the STT backend selection (whisper / openai /
/// groq). Kept as a `const` in this module so the delegate gate does
/// not have to reach into `rust_session_real_backends`, which is
/// itself feature-gated on `whisper-rs-local + rust-injection`.
const STT_BACKEND_ENV: &str = "VOICEPI_STT_BACKEND";

/// Env var carrying the privacy lock. When truthy, the runtime MUST
/// refuse to route audio to a non-loopback backend. Mirrors what the
/// Python worker's `vp_transcribe._assert_local_backend` used to
/// enforce before Wave 8 deleted that fallback path. Codex #441 P1
/// review round 3 (privacy leak).
const LOCAL_ONLY_ENV: &str = "VOICEPI_LOCAL_ONLY";

/// Env var carrying an explicit path to a GGML whisper.cpp model file.
/// Mirrors [`crate::whisper::MODEL_PATH_ENV`] but redeclared as a
/// module-local const so this file compiles on builds without
/// `whisper-rs-local` (the `pub` re-export is feature-gated). Codex
/// #441 P1 review round 3 (silent stub fallback).
const WHISPER_MODEL_PATH_ENV: &str = "VOICEPI_WHISPER_MODEL_PATH";

/// Resolve `key` against the same layered view the child worker will
/// see: the supervisor's `effective_command.env` overrides win first
/// (they materialise AppSettings on top of the parent env before
/// `Command::envs` is called), then we fall back to `std::env` so
/// process-env-only wiring (e.g. `VOICEPI_WHISPER_MODEL_PATH` set by
/// the parent binary at startup, or a debug knob exported in the
/// user's shell) still counts. Missing / unset lands on `None`.
pub(crate) fn resolved_env(env: &[(String, String)], key: &str) -> Option<String> {
    if let Some(value) = env
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.to_owned())
    {
        return Some(value);
    }
    std::env::var(key).ok()
}

/// Return true when a local Whisper backend can find a GGML model to
/// load. Mirrors the resolution rules of
/// [`crate::whisper::dispatch::resolve_model_path_from_env`] but
/// consults the effective env vec first so an AppSettings override
/// (materialised into `effective_command.env`) is seen exactly as the
/// child will see it. Codex #441 P1 review round 3 (silent stub
/// fallback).
///
/// Order of resolution:
///
/// 1. Explicit `VOICEPI_WHISPER_MODEL_PATH` override -- trusted at
///    face value (existence checked at load time; a bogus path fails
///    fast in `LocalWhisper::new`).
/// 2. Any catalog model whose SHA-verified file exists in the user
///    cache dir (`model_manager::CATALOG` + `is_downloaded`).
/// 3. Any custom `*.bin` / `*.gguf` file the user dropped into the
///    cache directory (`local_discovery::discover_models`).
///
/// Uses only always-compiled helpers (`model_manager` /
/// `local_discovery`) so the check compiles on stock builds without
/// `whisper-rs-local`. On such a build the gate short-circuits on
/// `all_required_features_enabled()` before ever calling this fn.
pub(crate) fn local_whisper_model_available(env: &[(String, String)]) -> bool {
    if let Some(raw) = resolved_env(env, WHISPER_MODEL_PATH_ENV) {
        if !raw.trim().is_empty() {
            return true;
        }
    }
    for entry in crate::whisper::model_manager::CATALOG {
        if crate::whisper::model_manager::is_downloaded(entry) {
            return true;
        }
    }
    if let Ok(dir) = crate::whisper::model_manager::models_cache_dir() {
        if !crate::whisper::local_discovery::discover_models(&dir).is_empty() {
            return true;
        }
    }
    false
}

/// Return `Some(reason)` when the effective env carries a
/// `VOICEPI_STT_BACKEND` value the worker-rust subprocess cannot
/// build. Returns `None` when the value is one of the recognised
/// backends -- unset / empty / `whisper` / `faster-whisper` (local
/// Whisper) OR `openai` / `groq` (cloud STT, Wave 5.5 gap #1 of #348).
///
/// `env` is the same effective command env passed to
/// [`should_delegate_to_worker_rust`] -- see that fn's docs for why
/// the gate cannot rely on `std::env` alone.
///
/// Named ish to match `should_delegate_to_worker_rust`'s "supervisor
/// stays on Python" fallback semantics: a `Some(reason)` return value
/// is exactly the message the supervisor logs before it declines to
/// delegate. Pure helper so the parse + set membership is unit-
/// testable without spawning a worker.
///
/// The parser mirrors [`super::rust_session_real_backends::resolve_stt_backend_from_env`]
/// (case-insensitive, trims whitespace, accepts the `faster-whisper`
/// alias) so the two decisions cannot drift -- a value the delegate
/// gate accepts is one the factory can build.
/// Codex #453 P2 (worker_rust.rs:276): rewrite any
/// `VOICEPI_STT_BACKEND=parakeet` (any casing) in the effective
/// command env to `"whisper"` so BOTH the delegate gate
/// ([`unsupported_worker_rust_settings_reason`]) AND the worker
/// child's backend resolver
/// (`rust_session_real_backends::resolve_stt_backend_from_env`) see
/// the migrated value. Without this the gate accepts the legacy
/// value (via its local normalisation) but the child spawns, resolves
/// the backend to `None`, and `build_production_sink` falls through
/// to the PR-4 stub session -- the worker looks alive but produces
/// empty transcriptions.
///
/// Pure helper: mutates the vec in place, idempotent, no-op when the
/// key is absent or already normal. Mirrors what
/// `AppSettings::from_value` does on the first save round-trip; this
/// helper handles the very first cold start post-upgrade before the
/// migration has landed on disk.
pub fn migrate_legacy_stt_backend_env(env: &mut [(String, String)]) {
    for (key, value) in env.iter_mut() {
        if key == STT_BACKEND_ENV && value.trim().eq_ignore_ascii_case("parakeet") {
            *value = "whisper".to_owned();
        }
    }
}

pub fn unsupported_worker_rust_settings_reason(env: &[(String, String)]) -> Option<String> {
    let raw = resolved_env(env, STT_BACKEND_ENV).unwrap_or_default();
    let mut normalised = raw.trim().to_ascii_lowercase();
    // Codex #453 P2: mirror the `AppSettings::from_value` migration
    // (Wave 8 of #348 dropped the Parakeet/NeMo backend) so an
    // upgraded user with `stt_backend = "parakeet"` in `config.json`
    // is not blocked from starting the ONLY remaining worker. The
    // save-round-trip in `save.rs` rewrites the file to `"whisper"`
    // eventually, but we normalise here too so the delegate gate does
    // not reject the value on the very first cold start post-upgrade.
    if normalised == "parakeet" {
        normalised = "whisper".to_owned();
    }
    match normalised.as_str() {
        "" | "whisper" | "faster-whisper" | "openai" | "groq" => {}
        other => {
            return Some(format!(
                "unsupported {STT_BACKEND_ENV}={other:?}; the rust-session worker \
                 knows whisper / openai / groq"
            ));
        }
    }

    // Codex #441 P1 review round 3 (silent stub fallback): a local
    // Whisper backend needs a resolvable GGML model. Without it
    // `make_real_session` fails, `build_production_sink` installs the
    // `StubTranscribeBackend` fallback, and every utterance produces
    // empty text -- the worker looks alive (emits `state=ready`) but
    // does no work. Fail-closed instead: the supervisor logs the
    // reason and refuses to spawn the worker.
    if matches!(normalised.as_str(), "" | "whisper" | "faster-whisper")
        && !local_whisper_model_available(env)
    {
        return Some(format!(
            "{WHISPER_MODEL_PATH_ENV} is unset and no GGML model was found in the \
             whisper-models cache directory; download one via \
             `whisper-dictate models download tiny.en`, drop a *.bin/*.gguf file \
             into the models cache directory, or set {WHISPER_MODEL_PATH_ENV} to \
             point at a whisper.cpp GGML model file"
        ));
    }

    // Codex #441 P1 review round 3 (privacy leak): `CloudTranscribeBackend`
    // POSTs the captured WAV to the configured base URL without
    // consulting VOICEPI_LOCAL_ONLY. Python's `_assert_local_backend`
    // used to refuse cloud STT under the privacy lock; Wave 8 removed
    // that fallback, so if the child spawns with the wrong config the
    // audio leaks to the internet before anything else notices. Refuse
    // to delegate here -- the supervisor logs the reason and the worker
    // exits, matching what `assert_local_backend` did in Python.
    if crate::dictate::env_gates::is_truthy(resolved_env(env, LOCAL_ONLY_ENV).as_deref())
        && matches!(normalised.as_str(), "openai" | "groq")
    {
        return Some(format!(
            "{LOCAL_ONLY_ENV}=1 forbids cloud STT ({STT_BACKEND_ENV}={raw:?}); \
             fix your config or unset {LOCAL_ONLY_ENV}"
        ));
    }

    None
}

/// Swap an existing Python-orchestrator [`WorkerCommand`] in place so
/// it spawns `<current-exe> worker-rust` instead. Preserves the
/// existing env (`VOICEPI_KEY`, `VOICEPI_TOGGLE`, `VOICEPI_LANG`,
/// model paths, ...) so the worker subprocess sees exactly the same
/// runtime configuration the Python worker would have seen.
///
/// `--stdin-only` is NOT added here -- production builds want the
/// real rdev listener. The integration test invokes the subcommand
/// with the flag explicitly.
///
/// Fails when `std::env::current_exe()` cannot be resolved (e.g. the
/// binary was deleted out from under us mid-run). The supervisor
/// surfaces the error and stays on Python in that case -- the caller
/// MUST also clear its own `delegate_to_worker_rust` flag on Err
/// (see [`plan_worker_rust_delegation`] for the composed helper that
/// gets this right).
pub fn swap_command_to_worker_rust(command: &mut WorkerCommand) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("cannot resolve current executable for worker-rust: {e}"))?;
    command.program = exe;
    command.args = vec!["worker-rust".to_owned()];
    Ok(())
}

/// Outcome of the pre-spawn delegate decision. Consumed by
/// [`crate::runtime::RuntimeSupervisor::start`]; extracted from
/// there so the "on swap failure the delegate flag AND the audio
/// bridge decision both fall back cleanly" branch is unit-testable
/// without spawning a real supervisor.
///
/// Claude review comment #3523185636 on PR #434: an earlier
/// iteration only reset `delegate_to_worker_rust = false` on failure
/// but left `use_rust_audio` at whatever `should_use_rust_audio_backend`
/// returned. If the user also opted into `VOICEPI_AUDIO_BACKEND=rust`
/// the (still-Python) child would spawn without
/// `--audio-source=rust-stdin` while the supervisor's audio bridge
/// wrote JSON frames into an unread pipe. This helper folds both
/// decisions together so the two flags cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelegatePlan {
    /// True iff the supervisor should spawn the worker-rust
    /// subprocess. False on the "stay on Python" path.
    pub delegate: bool,
    /// True iff the supervisor should push `--audio-source=rust-stdin`
    /// onto the Python worker's argv AND start the audio bridge. Always
    /// false on the delegate path (the subprocess owns cpal itself).
    pub push_rust_stdin_arg: bool,
}

/// Pure-logic decision for the supervisor's pre-spawn delegate
/// branch. Given the "user opted into delegation" flag, the "audio
/// backend requested" flag, and a boolean indicating whether
/// [`swap_command_to_worker_rust`] succeeded on the effective
/// command, returns the composed [`DelegatePlan`].
///
/// The supervisor calls this AFTER attempting the swap so we get the
/// success signal as a boolean rather than repeating the swap here
/// (the swap mutates `command` in place -- we can't run it twice).
pub fn plan_worker_rust_delegation(
    delegate_requested: bool,
    swap_succeeded: bool,
    rust_audio_requested: bool,
) -> DelegatePlan {
    if delegate_requested && swap_succeeded {
        // Delegating: subprocess owns audio.
        DelegatePlan {
            delegate: true,
            push_rust_stdin_arg: false,
        }
    } else {
        // Either the user did not opt in, or the swap failed and we
        // are falling back to Python. Either way, honour the audio
        // backend gate as if delegation had never been considered.
        DelegatePlan {
            delegate: false,
            push_rust_stdin_arg: rust_audio_requested,
        }
    }
}

/// Public CLI entry point. Wired from `src/rust/main.rs`'s clap
/// dispatcher. On stock builds (any feature missing) returns a clear
/// error so the caller -- usually the supervisor's launched
/// subprocess -- exits non-zero and the parent UI shows a useful
/// message rather than a silent hang.
///
/// `stdin_only=true` skips the rdev install (for the integration test
/// running in headless CI). Production callers leave it unset.
pub fn handle_worker_rust(stdin_only: bool) -> Result<()> {
    if !all_required_features_enabled() && !stdin_only {
        return Err(anyhow!(
            "this build of whisper-dictate was compiled without the full \
             rust-session feature set; rebuild with \
             `--features whisper-rs-local,rust-injection,audio-in-rust,rust-hotkeys` \
             to enable the in-process Rust dictation worker, OR pass \
             `--stdin-only` to run the test-driven session sink (stub \
             backends, no OS hotkey listener)"
        ));
    }
    // Always enable the worker-event gate for this process so the
    // session's emitter (which the supervisor's `parse_worker_event`
    // ingests) actually writes. Mirrors what
    // `build_production_sink` already does on its own -- doing it
    // here too lets us emit the startup `ready` event BEFORE the sink
    // is built, so the supervisor's parse loop sees a heartbeat even
    // if the sink construction itself fails.
    std::env::set_var(dictate_events::WORKER_EVENTS_ENV, "1");

    // Wave 8 regression mitigation (#462 follow-up): on Linux the in-process
    // cpal capture opens the default device, which on a PipeWire desktop routes
    // through libpipewire's realtime "data-loop" thread. rtkit grants that
    // thread SCHED_FIFO plus a per-thread RLIMIT_RTTIME; with the default small
    // quantum the data-loop overruns that budget during stream startup and the
    // kernel SIGKILLs the whole worker (~0.6 s after "ready" — no Rust panic,
    // no core dump, flaky race). Forcing a larger PipeWire quantum gives the
    // data-loop enough headroom per cycle to stay under the RT budget, which
    // empirically stops the kill (2048/48000 is the smallest stable value on
    // the reporter's Raptor Lake / SOF-HDA box; 4096 keeps margin). Only set it
    // when the user hasn't already tuned PipeWire themselves (via either
    // `PIPEWIRE_QUANTUM` or the per-stream `PIPEWIRE_LATENCY` knob), so an
    // explicit override always wins. The trade-off is a slightly larger capture
    // buffer (~85 ms), which is irrelevant for push-to-talk dictation.
    #[cfg(target_os = "linux")]
    if let Some(quantum) = desired_pipewire_quantum(
        std::env::var_os("PIPEWIRE_QUANTUM").as_deref(),
        std::env::var_os("PIPEWIRE_LATENCY").as_deref(),
    ) {
        std::env::set_var("PIPEWIRE_QUANTUM", quantum);
    }

    let runner = WorkerRunner::from_env(stdin_only)?;
    runner.run()
}

/// Decide the `PIPEWIRE_QUANTUM` value the Linux worker should force, or `None`
/// to leave the environment untouched. Returns the mitigation quantum only when
/// the user has NOT already tuned PipeWire — neither `PIPEWIRE_QUANTUM` nor the
/// per-stream `PIPEWIRE_LATENCY` knob is set — so any explicit user tuning
/// (e.g. a deliberately larger buffer) always wins over our crash-loop
/// mitigation. Pure (takes the two env values as arguments) so the defaulting
/// and override-wins behaviour is unit-testable without touching the process
/// environment.
#[cfg(target_os = "linux")]
fn desired_pipewire_quantum(
    existing_quantum: Option<&std::ffi::OsStr>,
    existing_latency: Option<&std::ffi::OsStr>,
) -> Option<&'static str> {
    if existing_quantum.is_some() || existing_latency.is_some() {
        None
    } else {
        Some("4096/48000")
    }
}

/// Bundle of runtime configuration that the worker thread needs.
/// Captured up-front from env vars / CLI flags so the main loop never
/// re-reads the env mid-flight (the values are also passed down to
/// the coordinator + session sink via the existing wiring -- this
/// struct is a thin local cache for the bits the worker mainloop
/// itself consults).
struct WorkerRunner {
    /// Pre-parsed PTT chord names from `VOICEPI_KEY` (split on `+`,
    /// trimmed, empties dropped). Empty when no chord is configured;
    /// production builds without a chord refuse to install the rdev
    /// listener (PTT is silent otherwise), test builds tolerate it
    /// because they drive the coordinator from stdin.
    key_names: Vec<String>,
    /// Hold-to-talk vs. toggle mode parsed from `VOICEPI_TOGGLE`.
    mode: Mode,
    /// When true, skip the rdev install and drive the coordinator
    /// from stdin only. Set by the integration test.
    stdin_only: bool,
}

impl WorkerRunner {
    fn from_env(stdin_only: bool) -> Result<Self> {
        // Re-use the supervisor's existing helpers so the env-var
        // semantics stay in lock-step with the Python orchestrator
        // path (one source of truth per knob).
        let dummy = WorkerCommand {
            program: std::path::PathBuf::new(),
            args: Vec::new(),
            working_dir: std::path::PathBuf::new(),
            env: std::env::vars().collect(),
        };
        let key_names = extract_hotkey_key_names(&dummy);
        let toggle = std::env::var("VOICEPI_TOGGLE")
            .ok()
            .as_deref()
            .map(parse_toggle_value)
            .unwrap_or(false);
        let mode = if toggle {
            Mode::Toggle
        } else {
            Mode::HoldToTalk
        };
        Ok(Self {
            key_names,
            mode,
            stdin_only,
        })
    }

    /// Drive the worker mainloop until stdin EOF / `quit` / fatal error.
    fn run(self) -> Result<()> {
        let stderr = io::stderr();

        // Emit a heartbeat the supervisor's `parse_worker_event` consumer
        // can latch onto BEFORE the session sink builds. Without this,
        // the first observable worker event would come from the session
        // itself (no startup ready), so the supervisor's `state=ready`
        // wait (audio bridge / UI status badge) would not fire until the
        // first PTT press. Matches the Python orchestrator's behaviour
        // (`runtime.py` emits `status=ready` once the model is loaded).
        emit_worker_ready(&mut stderr.lock())?;

        // Build the production session sink. On a stock build this
        // falls back to the PR 4 stubs (transcribe -> empty text,
        // inject -> no-op) and there's no audio pump; production
        // builds get the real backends + cpal pump.
        let (tx, rx) = mpsc::channel::<RuntimeEvent>();
        // Codex #453 P2 (runtime.rs:662): use the strict variant so a
        // real-backend init failure (bogus VOICEPI_WHISPER_MODEL_PATH,
        // audio pump / VAD / cpal init error, ...) makes the child
        // exit non-zero instead of silently downgrading to the PR-4
        // stub session. The supervisor's Err path surfaces a clean
        // error message; the alternative (worker looks alive, every
        // utterance produces empty text) is a shipping-blocker post-
        // Wave 8 because there is no Python fallback to unstick it.
        let (action_sink, coord_slot) = if self.stdin_only {
            // Integration test path -- runs on stock builds too and
            // WANTS the stub sink so the coordinator wire-up is
            // exercised end-to-end without a real GGML model.
            rust_session_sink::build_production_sink(tx.clone(), None)
        } else {
            rust_session_sink::try_build_real_production_sink(tx.clone(), None)?
        };

        // Spawn the coordinator with the session sink as its action sink.
        let (coord_handle, coord_thread) =
            spawn_coordinator(Options { mode: self.mode }, action_sink, Instant::now);
        // Make the coordinator handle available to the sink so its
        // `processing_finished` callback can drive the coordinator out
        // of `Stage::Processing` after a stop completes. `OnceLock::set`
        // returns Err on a second call -- we are the only writer so
        // this is unreachable in practice, but match the supervisor's
        // belt-and-braces path.
        if coord_slot.set(coord_handle.clone()).is_err() {
            eprintln!(
                "[worker-rust] coordinator slot already populated; ignoring \
                 (programming error if this fires more than once)"
            );
        }

        // Optionally install the rdev OS listener. Skipped on
        // `--stdin-only` (integration test); production builds MUST
        // succeed here or the worker exits non-zero so the supervisor
        // surfaces the failure. Codex #441 P1 (comment #3550913227):
        // an earlier iteration returned None and fell through to the
        // stdin loop, which meant PTT went silently dead while the
        // subprocess kept blocking on an unwritten pipe.
        let hotkey_handle = if self.stdin_only {
            None
        } else {
            match install_listener(self.key_names.clone(), self.mode, coord_handle.clone()) {
                Some(handle) => Some(handle),
                None => {
                    // Tear the (already-built) coordinator + sink down so
                    // captured resources drop cleanly, then bail. The
                    // `emit_hotkey_install_failed` worker-event has
                    // already fired from `install_listener`, so the
                    // supervisor logs the specific failure reason. The
                    // event pump hasn't spawned yet (`pump = ...` runs
                    // below), so we only need to close its rx side by
                    // dropping `tx`.
                    coord_handle.shutdown();
                    coord_thread.join();
                    drop(tx);
                    return Err(anyhow!(
                        "worker-rust cannot install the OS hotkey listener;                          exiting so the supervisor can surface the failure                          (PTT would be silently dead otherwise)"
                    ));
                }
            }
        };

        // Pump events from the channel to stderr/stdout in a background
        // thread so the main thread is free to read stdin commands.
        let pump = spawn_event_pump(rx);

        // Drive stdin commands on the main thread. EOF / `quit` returns;
        // unknown lines are logged and ignored.
        run_stdin_loop(&coord_handle);

        // Shut everything down in reverse order. The coordinator joins
        // first so any in-flight stop_and_transcribe completes before
        // the sink (and its captured session / audio pump) drops; the
        // event pump then drains any tail events and exits when the
        // channel disconnects.
        coord_handle.shutdown();
        coord_thread.join();
        drop(tx); // close the supervisor side of the event channel
        let _ = pump.join();
        if let Some(handle) = hotkey_handle {
            handle.shutdown();
        }

        Ok(())
    }
}

/// Install the rdev OS hotkey listener via
/// [`crate::hotkey::install_hotkey`] directly, BYPASSING the
/// [`crate::runtime::maybe_install_rust_hotkey`] env-var gate.
///
/// # Why the env-var gate is skipped (Claude review comment #3523185556)
///
/// `maybe_install_rust_hotkey` first checks
/// [`crate::hotkey::rust_hotkey_backend_requested`] (i.e.
/// `VOICEPI_HOTKEY_BACKEND=rust`) and returns `None` when the env var
/// isn't set -- that's the supervisor-side gate: the supervisor only
/// installs the Rust listener when the user OPTED IN specifically for
/// the hotkey backend.
///
/// But by the time `install_listener` runs we're already inside the
/// worker-rust subprocess, which the supervisor only spawns when
/// [`should_delegate_to_worker_rust`] is true -- i.e. the user
/// already opted into the rust-session backend. Delegation itself
/// implies "the subprocess owns the hotkey lifecycle", so gating on a
/// second, separately-named env var here would silently break PTT for
/// any user who set `VOICEPI_DICTATE_BACKEND=rust-session` without
/// also setting `VOICEPI_HOTKEY_BACKEND=rust` -- the supervisor
/// wouldn't spawn Python (it delegated), the subprocess wouldn't
/// install rdev (env var missing), and PTT would be dead with no
/// visible error.
///
/// Calling `install_hotkey` directly here treats "we're running as
/// worker-rust" as the implicit opt-in, matching the semantics the
/// supervisor's delegation already committed to.
///
/// # Bridge closure
///
/// `install_hotkey` requires an action-sink callback. The rdev
/// manager thread emits `TrackerOutput`s (chord press / release /
/// cancel) which `install_hotkey`'s internal coordinator translates
/// into [`crate::hotkey::coordinator::CoordinatorAction`]s and hands
/// to the sink. We translate those actions back into
/// [`CoordinatorEvent`]s on the session-sink coordinator we spawned
/// manually in [`WorkerRunner::run`], so the session sink reacts to
/// them. A follow-up will let `install_hotkey` share the caller's
/// coordinator handle so the second internal coordinator isn't
/// needed.
fn install_listener(
    key_names: Vec<String>,
    mode: Mode,
    session_coord: CoordinatorHandle,
) -> Option<crate::hotkey::HotkeyHandle> {
    if key_names.is_empty() {
        eprintln!(
            "[worker-rust] no PTT chord configured (VOICEPI_KEY unset or empty); \
             rdev listener not installed -- worker is driven from stdin only"
        );
        return None;
    }
    use crate::hotkey::coordinator::CoordinatorAction;
    use crate::hotkey::{install_hotkey, HotkeyConfig, InstallError};
    let bridge = session_coord;
    let cfg = HotkeyConfig { key_names, mode };
    match install_hotkey(cfg, move |action| {
        let event = match action {
            CoordinatorAction::StartRecording(_) => CoordinatorEvent::Press,
            CoordinatorAction::StopAndTranscribe(_) => CoordinatorEvent::Release,
            CoordinatorAction::CancelRecording(_) => CoordinatorEvent::Cancel,
        };
        bridge.send(event);
    }) {
        Ok(handle) => {
            eprintln!(
                "[worker-rust] rdev hotkey listener installed; PTT chord will drive the \
                 in-process DictateSession"
            );
            Some(handle)
        }
        Err(InstallError::Unsupported) => {
            // Only reachable on a build without the `rust-hotkeys`
            // feature. `handle_worker_rust` refuses to run in that
            // configuration unless `--stdin-only` is set, which
            // itself bypasses `install_listener` (see
            // `WorkerRunner::run`). Kept explicit so a future
            // refactor that widens the entry-point contract still
            // surfaces this branch cleanly instead of silently
            // going PTT-dark.
            eprintln!(
                "[worker-rust] rdev listener unavailable: build was compiled without \
                 --features rust-hotkeys; PTT chord will not work in this subprocess"
            );
            // PR #441 review round 2 (Codex P1 finding 4): also emit a
            // machine-parseable worker-event so the supervisor can log
            // a prominent diagnostic pointing users to the
            // `python-legacy` escape hatch. Actual auto-fallback (kill
            // the child + respawn Python) is a Wave-5.5 gap tracked
            // separately -- this signal is the first step.
            emit_hotkey_install_failed("rust-hotkeys feature not compiled in");
            None
        }
        Err(err) => {
            eprintln!(
                "[worker-rust] hotkey install failed: {err}; PTT chord will not \
                 work in this subprocess (X11: missing display / accessibility \
                 permission; Wayland/evdev: user not in the `input` group?)"
            );
            // PR #441 review round 2 (Codex P1 finding 4): signal
            // failure to the supervisor via a worker-event so it can
            // surface the escape-hatch hint even if the user missed
            // the plain-stderr warning above.
            emit_hotkey_install_failed(&format!("{err}"));
            None
        }
    }
}

/// Emit a one-shot `[worker-event] {"event":"status","state":"ready"}`
/// line on stderr so the supervisor knows the worker is alive BEFORE
/// any PTT press. Mirrors the Python orchestrator's startup `ready`
/// event (`runtime.py::_emit_worker_event(..., state="ready")`).
fn emit_worker_ready<W: Write>(writer: &mut W) -> Result<()> {
    let event = dictate_events::StatusEvent::new(dictate_events::WorkerStatus::Ready);
    dictate_events::emit_status(writer, &event)?;
    Ok(())
}

/// Machine-parseable wire signal for "the worker's rdev hotkey install
/// just failed". Emits a `[worker-event]` line on stderr with
/// `state=error` and `reason=hotkey_install_failed` so the parent
/// supervisor's `parse_worker_event` consumer can detect the failure
/// and log a prominent hint about
/// `VOICEPI_DICTATE_BACKEND=python-legacy`. PR #441 review round 2
/// (Codex P1 finding 4) added this alongside the pre-existing plain
/// `eprintln!` so a supervisor that missed the raw stderr line still
/// gets a structured signal it can react to.
///
/// Wave-5.5 will turn this into an actual auto-fallback (kill child +
/// respawn Python); for PR #441 the supervisor only needs to log the
/// signal so users see the escape hatch.
fn emit_hotkey_install_failed(detail: &str) {
    let mut event = dictate_events::StatusEvent::new(dictate_events::WorkerStatus::Error);
    event.extras.insert(
        "reason".to_owned(),
        serde_json::Value::from("hotkey_install_failed"),
    );
    event
        .extras
        .insert("detail".to_owned(), serde_json::Value::from(detail));
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    // Best-effort: if stderr is dead we cannot signal anyway.
    let _ = dictate_events::emit_status(&mut lock, &event);
}

/// Spawn the event-pump thread that forwards [`RuntimeEvent`]s off
/// the channel to stderr/stdout. Returns the join handle so the
/// mainloop can wait for the pump to drain on shutdown.
///
/// Routing mirrors the supervisor's existing line decoders:
///
/// * [`RuntimeEvent::Worker`] -- re-serialised as a
///   `[worker-event] {...}\n` line on stderr so the parent
///   supervisor's `parse_worker_event` consumer round-trips them
///   unchanged.
/// * [`RuntimeEvent::Stderr`] / [`RuntimeEvent::Error`] -- written
///   to stderr verbatim (no `[worker-event]` prefix).
/// * [`RuntimeEvent::Stdout`] -- written to stdout verbatim.
/// * [`RuntimeEvent::Started`] / [`RuntimeEvent::Exited`] -- ignored
///   (the session sink never emits these; included for total-match
///   exhaustiveness).
fn spawn_event_pump(rx: mpsc::Receiver<RuntimeEvent>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("worker-rust-event-pump".to_owned())
        .spawn(move || {
            let mut stderr = io::stderr().lock();
            let mut stdout = io::stdout().lock();
            while let Ok(event) = rx.recv() {
                forward_event(event, &mut stderr, &mut stdout);
            }
        })
        .expect("worker-rust-event-pump spawn")
}

/// Pure-logic helper for [`spawn_event_pump`] so the routing is
/// unit-testable without a real channel / thread.
fn forward_event<E: Write, O: Write>(event: RuntimeEvent, stderr: &mut E, stdout: &mut O) {
    match event {
        RuntimeEvent::Worker(worker) => {
            // Re-serialise the worker event back into the wire form
            // the supervisor expects. The payload Value is already
            // shaped correctly (event + state + extras); we just
            // bracket it with the prefix and newline.
            let line = format!(
                "{}{}\n",
                dictate_events::WORKER_EVENT_PREFIX,
                worker.payload
            );
            let _ = stderr.write_all(line.as_bytes());
            let _ = stderr.flush();
        }
        RuntimeEvent::Stderr(line) => {
            let _ = writeln!(stderr, "{line}");
        }
        RuntimeEvent::Error(line) => {
            let _ = writeln!(stderr, "[worker-rust] error: {line}");
        }
        RuntimeEvent::Stdout(line) => {
            let _ = writeln!(stdout, "{line}");
        }
        RuntimeEvent::Started { .. } | RuntimeEvent::Exited { .. } => {
            // Not emitted by the session sink; only the supervisor's
            // own bookkeeping produces these. Worker-rust runs WITHOUT
            // a supervisor underneath it, so this branch is unreachable
            // in practice. Drop silently to keep the match exhaustive.
        }
    }
}

/// Drive the stdin command loop. Blocks the calling thread until EOF
/// or an explicit `quit` line. Each iteration trims whitespace,
/// lower-cases the command for case-insensitive matching, and
/// dispatches:
///
/// * `press`  -> [`CoordinatorEvent::Press`]
/// * `release` -> [`CoordinatorEvent::Release`]
/// * `cancel` -> [`CoordinatorEvent::Cancel`]
/// * `quit` / `exit` -> return (graceful shutdown)
/// * empty line -> ignored
/// * anything else -> stderr warning, continue
///
/// Read errors are treated as EOF: the supervisor closing stdin (or
/// the integration test ending its writes) is the canonical shutdown
/// signal on Windows where SIGTERM isn't reliable.
fn run_stdin_loop(coord: &CoordinatorHandle) {
    let stdin = io::stdin();
    let reader = BufReader::new(stdin.lock());
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("[worker-rust] stdin read error: {err}; shutting down");
                return;
            }
        };
        let cmd = line.trim().to_ascii_lowercase();
        match dispatch_stdin_command(&cmd, coord) {
            StdinCommandOutcome::Continue => continue,
            StdinCommandOutcome::Quit => return,
        }
    }
    // EOF on stdin -> graceful shutdown (same as `quit`).
}

/// Outcome of a single stdin command dispatch. Extracted so the
/// command parser is unit-testable without a real stdin handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdinCommandOutcome {
    Continue,
    Quit,
}

/// Pure-logic dispatcher for one trimmed + lowercased stdin command.
/// Returns whether the mainloop should keep reading or shut down.
/// Side effect: sends to `coord` on press / release / cancel.
fn dispatch_stdin_command(cmd: &str, coord: &CoordinatorHandle) -> StdinCommandOutcome {
    match cmd {
        "" => StdinCommandOutcome::Continue,
        "press" => {
            coord.send(CoordinatorEvent::Press);
            StdinCommandOutcome::Continue
        }
        "release" => {
            coord.send(CoordinatorEvent::Release);
            StdinCommandOutcome::Continue
        }
        "cancel" => {
            coord.send(CoordinatorEvent::Cancel);
            StdinCommandOutcome::Continue
        }
        "quit" | "exit" => StdinCommandOutcome::Quit,
        other => {
            eprintln!(
                "[worker-rust] unknown stdin command: {other:?} (expected one of \
                 press/release/cancel/quit); ignoring"
            );
            StdinCommandOutcome::Continue
        }
    }
}
