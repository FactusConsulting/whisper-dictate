//! In-process Rust dictation dispatch: Phase B of audit item 5.
//!
//! When the operator opts in with `VOICEPI_DICTATE_ENGINE=rust`, the
//! [`super::supervisor::RuntimeSupervisor::start`] entry point installs
//! the full Rust dictation runtime (hotkey listener + coordinator +
//! session sink + real backends when the required features are
//! compiled in) inside the UI process itself, instead of spawning a
//! Python worker child — removing the Phase A `whisper-dictate
//! dictate-run` subprocess from the runtime supervision ladder.
//!
//! See `docs/design/item5-phase-b-inprocess.md` for the design and the
//! five risks (config-parsing drift, status-event parity, panic
//! containment, model-load UX, env-var nomenclature) this module
//! addresses.
//!
//! ## Env-var contract
//!
//! * [`ENGINE_ENV`] (`VOICEPI_DICTATE_ENGINE`) — Phase A/B switch:
//!   [`ENGINE_PYTHON`] (default), [`ENGINE_RUST`] (in-process).
//!   Case-insensitive; blank / unknown values fall back to Python
//!   with a stderr warning.
//! * `VOICEPI_DICTATE_BACKEND=rust-session` — older lower-level opt-in.
//!   When set alongside `VOICEPI_DICTATE_ENGINE=rust`, ENGINE wins
//!   (design doc risk #5) and an informational stderr line names the
//!   effective backend.
//!
//! ## Failure model
//!
//! Any [`InProcessInstallError`] is a fallback signal — the supervisor
//! logs a stderr line naming the reason and spawns the Python worker
//! with `VOICEPI_DICTATE_ENGINE` cleared for the child so it does not
//! re-enter Phase A's subprocess pipeline. Feature-gated on both
//! `rust-hotkeys` and `rust-injection`; [`try_install`] wraps setup
//! in [`std::panic::catch_unwind`] so a panic at the install boundary
//! surfaces as [`InProcessInstallError::Panicked`] rather than
//! aborting the UI process. Panics AFTER install (on coordinator /
//! manager threads) still abort — that scope is intentionally
//! "install boundary" only.

use std::sync::mpsc::Sender;

use super::supervisor::{RuntimeEvent, WorkerEvent};
use super::worker_command::WorkerCommand;

// ── env-var gate ─────────────────────────────────────────────────────────────

/// Canonical env var name for the Phase A/B engine switch. Matches the
/// Python-side constant at
/// `src/python/whisper_dictate/vp_dictate_engine.py::ENGINE_ENV` — kept in
/// sync manually rather than through a shared constant because the two
/// languages ship in separate build systems.
pub(crate) const ENGINE_ENV: &str = "VOICEPI_DICTATE_ENGINE";

/// Default value — runs the Python `Dictate(...).run()` loop unchanged.
pub(crate) const ENGINE_PYTHON: &str = "python";

/// Opt-in value — installs the Rust runtime in-process.
pub(crate) const ENGINE_RUST: &str = "rust";

/// Canonical resolution of the raw `VOICEPI_DICTATE_ENGINE` env value.
/// Returns [`EngineChoice::Unknown`] for anything the caller must warn
/// on (so `_dispatch_engine`-style callers can log a one-liner before
/// falling back). Unset / empty resolves to [`EngineChoice::Python`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EngineChoice {
    /// Env var unset, empty, or explicitly `python`.
    Python,
    /// Env var explicitly `rust`.
    Rust,
    /// Env var set to something other than the known values. The
    /// supervisor logs a stderr warning naming the raw value and falls
    /// through to the Python engine — same behaviour as the Python
    /// dispatcher in `runtime.py::_dispatch_engine`.
    Unknown(String),
}

impl EngineChoice {
    /// Resolve from an arbitrary env accessor. Exposed for tests that
    /// need a hermetic value without touching process env.
    pub(crate) fn from_env_value(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or("") {
            "" => Self::Python,
            v if v.eq_ignore_ascii_case(ENGINE_PYTHON) => Self::Python,
            v if v.eq_ignore_ascii_case(ENGINE_RUST) => Self::Rust,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

/// Resolve the current process's engine choice from
/// [`ENGINE_ENV`]. Convenience wrapper around
/// [`EngineChoice::from_env_value`] for supervisor-side callers.
pub(crate) fn engine_choice_from_env() -> EngineChoice {
    let raw = std::env::var(ENGINE_ENV).ok();
    EngineChoice::from_env_value(raw.as_deref())
}

// ── feature availability gate ────────────────────────────────────────────────

/// Whether this build carries the features the in-process runtime needs
/// (`rust-hotkeys` + `rust-injection`). Mirrors
/// [`super::dictate_run::features_available`] — kept as a distinct const
/// so a future refactor that widens the in-process gate (e.g. adding
/// `audio-in-rust`) can move independently of the CLI verb.
///
/// `#[allow(dead_code)]` because on a stock build only the tests
/// reference this — `try_install` itself is `#[cfg]`-gated to a stub
/// that returns [`InProcessInstallError::FeaturesMissing`] directly.
#[allow(dead_code)]
pub(crate) const fn features_available() -> bool {
    cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
}

// ── install-time errors ──────────────────────────────────────────────────────

/// Reason [`try_install`] refused. Each variant maps to a specific
/// stderr line the supervisor emits before falling back to Python.
#[derive(Debug)]
#[allow(dead_code)] // Panicked / HotkeyInstallFailed only construct on feature builds
pub(crate) enum InProcessInstallError {
    /// Build was not compiled with `rust-hotkeys` + `rust-injection`.
    /// Actionable message names the rebuild command.
    FeaturesMissing,
    /// Config load failed (`config::load_settings`). Wraps the anyhow
    /// error string; the supervisor forwards it verbatim to stderr.
    ConfigLoadFailed(String),
    /// Config's PTT `settings.key` was empty. Same message shape as the
    /// `dictate-run` verb so users get consistent guidance.
    EmptyChord,
    /// Real Rust backend refused: missing feature, cpal device
    /// unavailable, model resolution failed, Silero ONNX missing.
    /// Wraps the reason from `try_build_production_sink`; triggers
    /// Python fallback (Codex P1 PR #519 in_process.rs:373).
    MissingBackend(String),
    /// [`crate::hotkey::install_hotkey`] failed. Wraps the underlying
    /// [`crate::hotkey::InstallError`] message; keeps the supervisor
    /// independent of the concrete error variants (they may grow).
    HotkeyInstallFailed(String),
    /// [`std::panic::catch_unwind`] caught a panic during install.
    /// Payload is a best-effort stringification of the panic message.
    Panicked(String),
}

impl std::fmt::Display for InProcessInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FeaturesMissing => write!(
                f,
                "in-process Rust runtime needs `rust-hotkeys` + `rust-injection` \
                 (rebuild with `cargo build --features rust-hotkeys,rust-injection`); \
                 falling back to the Python worker"
            ),
            Self::ConfigLoadFailed(msg) => write!(
                f,
                "in-process Rust runtime could not load config ({msg}); \
                 falling back to the Python worker"
            ),
            Self::EmptyChord => write!(
                f,
                "in-process Rust runtime refused: no PTT chord configured \
                 (settings.key is empty); set one via \
                 `whisper-dictate config set key ctrl_l+shift_l` and retry. \
                 Falling back to the Python worker for this run"
            ),
            Self::MissingBackend(msg) => write!(
                f,
                "in-process Rust runtime cannot serve PTT ({msg}); \
                 falling back to the Python worker. Rebuild with the \
                 `whisper-rs-local`, `rust-injection`, and `audio-in-rust` \
                 cargo features and download a Whisper model to enable \
                 the in-process path"
            ),
            Self::HotkeyInstallFailed(msg) => write!(
                f,
                "in-process Rust hotkey install failed ({msg}); \
                 falling back to the Python worker"
            ),
            Self::Panicked(msg) => write!(
                f,
                "in-process Rust runtime install panicked ({msg}); \
                 falling back to the Python worker. This is a bug - please file \
                 an issue at https://github.com/lars-frost/whisper-dictate/issues"
            ),
        }
    }
}

// ── worker-event helpers ─────────────────────────────────────────────────────

/// Emit the same `worker_ready` status event the Python worker emits
/// on model-load completion, so the UI's ready latch fires identically
/// when the in-process Rust engine took over. Runs on the supervisor's
/// own thread so a slow model load does not freeze the UI thread —
/// callers already spawn model construction on this thread (mitigation
/// for design doc risk #4).
///
/// The payload mirrors the shape [`super::rust_session_sink`] and the
/// Python `_emit_worker_event("status", state="ready", ...)` produce:
/// `{"event":"status","state":"ready","engine":"rust"}`. The `engine`
/// key is Phase B specific so the UI's log-view (and support-thread
/// grep) can tell an in-process ready apart from a Python ready.
pub(crate) fn emit_ready_worker_event(tx: &Sender<RuntimeEvent>) {
    let payload = serde_json::json!({
        "event": "status",
        "state": "ready",
        "engine": "rust",
    });
    let _ = tx.send(RuntimeEvent::Worker(WorkerEvent {
        event: "status".to_owned(),
        state: Some("ready".to_owned()),
        payload,
    }));
}

/// Emit the informational stderr line the design doc requires when
/// the operator has both [`ENGINE_ENV`] AND
/// `VOICEPI_DICTATE_BACKEND=rust-session` set simultaneously. Names the
/// effective backend so the operator knows which flag won (design doc
/// risk #5). Called from
/// [`super::supervisor::RuntimeSupervisor::start`] just before the
/// install attempt.
pub(crate) fn maybe_emit_env_precedence_note(tx: &Sender<RuntimeEvent>) {
    if super::rust_session_sink::dictate_backend_rust_session_requested() {
        let _ = tx.send(RuntimeEvent::Stderr(format!(
            "[runtime] both {ENGINE_ENV}=rust and VOICEPI_DICTATE_BACKEND=rust-session are set; \
             {ENGINE_ENV} wins and drives the in-process runtime (VOICEPI_DICTATE_BACKEND is \
             ignored for this session)"
        )));
    }
}

// ── install path (feature-gated) ─────────────────────────────────────────────

/// Feature-gated install: on a stock build this immediately returns
/// [`InProcessInstallError::FeaturesMissing`] so the supervisor's
/// caller can fall through to Python; on a feature-complete build it
/// delegates to [`install_supported`] wrapped in
/// [`std::panic::catch_unwind`] so a panic in the setup path is
/// converted into a recoverable [`InProcessInstallError::Panicked`]
/// rather than aborting the UI process (design doc risk #3).
///
/// On success the caller receives an
/// [`InProcessInstallation`] carrying the live
/// [`crate::hotkey::HotkeyHandle`] AND the shared
/// [`crate::hotkey::coordinator::CoordinatorHandle`] slot the session
/// sink populated. The supervisor MUST park the live handle in
/// `RuntimeSupervisor::hotkey_handle` so the coordinator threads
/// survive across restart() calls (same rationale as the existing
/// [`super::hotkey_install::install_rust_hotkey_from_command`] path).
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(crate) fn try_install(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) -> std::result::Result<InProcessInstallation, InProcessInstallError> {
    // Panic containment (design doc risk #3). AssertUnwindSafe is
    // required because `Sender<RuntimeEvent>` is not by default UnwindSafe
    // — the supervisor owns its own clone and any partial `send` before
    // the panic is a no-op the receiver will ignore.
    let install_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        install_supported(tx.clone(), repaint_notifier)
    }));
    match install_result {
        Ok(Ok(installation)) => Ok(installation),
        Ok(Err(err)) => Err(err),
        Err(panic_payload) => Err(InProcessInstallError::Panicked(stringify_panic(
            panic_payload,
        ))),
    }
}

/// Stock-build stub — always returns [`InProcessInstallError::FeaturesMissing`]
/// so the supervisor's caller falls back to the Python worker without ever
/// spinning up any threads. The `tx` / `repaint_notifier` args are consumed
/// as `_` so the call shape stays identical across feature configurations.
#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
pub(crate) fn try_install(
    _tx: Sender<RuntimeEvent>,
    _repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) -> std::result::Result<InProcessInstallation, InProcessInstallError> {
    Err(InProcessInstallError::FeaturesMissing)
}

/// Best-effort stringification of `catch_unwind`'s [`Any`] payload.
/// The stdlib guarantees `&'static str` and `String` payloads for
/// `panic!()` invocations that pass a literal or a formatted string;
/// anything else lands as a placeholder so the caller still gets a
/// useful log line rather than a `{:?}` dump of an opaque `Any`.
///
/// Always compiled (not feature-gated) so the unit test at the bottom
/// of this module can pin the panic → string conversion without
/// requiring `rust-hotkeys+rust-injection` — the stringifier itself
/// carries no OS surface. `#[allow(dead_code)]` because on stock builds
/// only the test calls it.
#[allow(dead_code)]
pub(crate) fn stringify_panic(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<opaque panic payload>".to_owned()
    }
}

/// Live in-process installation: caller holds the handle for the
/// lifetime of the process (the manager + coordinator threads cannot
/// be cleanly stopped, matching the Python-worker path in
/// [`super::supervisor::RuntimeSupervisor`]).
///
/// The `_coord_slot_keepalive` field pins the shared
/// [`std::sync::OnceLock<CoordinatorHandle>`] the session sink populated
/// so the sink's `processing_finished` callback keeps working when the
/// coordinator fires `StopAndTranscribe` — dropping the slot here would
/// leave the coordinator stuck in `Stage::Processing` on the second
/// PTT press.
///
/// Not `#[derive(Debug)]` because the handle contains a raw ptr through
/// the OS listener thread; nothing in the codebase formats it.
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(crate) struct InProcessInstallation {
    pub(crate) hotkey_handle: crate::hotkey::HotkeyHandle,
    /// Kept alive so the session sink's `on_processing_finished`
    /// callback survives; the callback captures a clone of the same
    /// `Arc<OnceLock<_>>` and reads the slot every stop.
    #[allow(dead_code)]
    pub(crate) coord_slot_keepalive:
        std::sync::Arc<std::sync::OnceLock<crate::hotkey::coordinator::CoordinatorHandle>>,
}

/// Stub type-alias so the stock-build call path type-checks even
/// though [`try_install`] returns Err before ever constructing one.
/// Fields kept private to prevent stock-build callers from
/// constructing an empty stand-in accidentally.
#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
pub(crate) struct InProcessInstallation {
    _private: (),
}

/// Feature-complete install path. Mirrors the setup body of
/// [`super::dictate_run::run`] (`src/rust/runtime/dictate_run.rs`) but
/// stops short of running the event loop — the supervisor owns the
/// loop via its own [`super::supervisor::RuntimeSupervisor::poll`] pump
/// and the coordinator drives worker events through the same `tx` the
/// Python-worker path uses. Sharing the setup with the CLI verb keeps
/// the two behavioural code paths byte-identical for anything the
/// supervisor observes.
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
fn install_supported(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
) -> std::result::Result<InProcessInstallation, InProcessInstallError> {
    use crate::config::load_settings;
    use crate::hotkey::{coordinator, install_hotkey, HotkeyConfig, InstallError};

    // 1. Load config through the same resolver the `dictate-run` CLI
    //    verb uses (design doc risk #1: config-parsing drift). The
    //    supervisor here does NOT honour `--config PATH` because the UI
    //    process has no CLI arg surface; VOICEPI_CONFIG is the only
    //    override, and `load_settings` reads it internally.
    let settings =
        load_settings().map_err(|err| InProcessInstallError::ConfigLoadFailed(err.to_string()))?;
    let key_names = split_key_names(&settings.key);
    if key_names.is_empty() {
        return Err(InProcessInstallError::EmptyChord);
    }
    let mode = if settings.toggle_mode {
        coordinator::Mode::Toggle
    } else {
        coordinator::Mode::HoldToTalk
    };

    // 2. Build the REAL production session sink. The strict variant
    //    returns Err when the whisper + inject session cannot be
    //    constructed; that Err becomes `MissingBackend`, which
    //    triggers the supervisor's Python-worker fallback. Without
    //    this the silent-stub fallback in the historical
    //    `build_production_sink` would leave a no-op sink installed
    //    and the advertised auto-fallback would never fire (Codex P1
    //    PR #519 in_process.rs:373).
    let (sink, coord_slot) =
        super::rust_session_sink::try_build_production_sink(tx.clone(), repaint_notifier)
            .map_err(InProcessInstallError::MissingBackend)?;

    // 3. Install the hotkey with the sink as the action target. Wraps
    //    `install_hotkey`'s per-error variants into a single
    //    fallback-eligible `HotkeyInstallFailed` so the supervisor's
    //    caller does not need to know the hotkey error taxonomy.
    let handle =
        install_hotkey(HotkeyConfig { key_names, mode }, sink).map_err(|err| match err {
            InstallError::Unsupported => InProcessInstallError::FeaturesMissing,
            other => InProcessInstallError::HotkeyInstallFailed(other.to_string()),
        })?;

    // 4. Wire the coordinator handle back into the sink's OnceLock so
    //    `on_processing_finished` can send `ProcessingFinished(id)` when
    //    a stop completes — otherwise the coordinator stays parked in
    //    `Stage::Processing` and the next press is ignored. Same shape
    //    as `install_session_sink_hotkey` in `hotkey_install.rs`; a
    //    duplicate-set is a refactor regression signal, not fatal.
    if coord_slot.set(handle.coordinator_handle()).is_err() {
        let _ = tx.send(RuntimeEvent::Stderr(
            "[in-process] coordinator handle slot already populated; \
             ignoring (this indicates a refactor regression but is not fatal)"
                .to_owned(),
        ));
    }

    Ok(InProcessInstallation {
        hotkey_handle: handle,
        coord_slot_keepalive: coord_slot,
    })
}

/// Feature-gated helper: load the current PTT key names from
/// `config::load_settings` for the supervisor's restart-path resume.
/// Returns the raw string on error so the supervisor can wrap it in
/// [`InProcessInstallError::ConfigLoadFailed`] with the same shape the
/// initial install produces.
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(crate) fn resume_key_names_from_env() -> std::result::Result<Vec<String>, String> {
    let settings = crate::config::load_settings().map_err(|err| err.to_string())?;
    Ok(split_key_names(&settings.key))
}

/// Stock-build stub — the supervisor's restart-path only reaches this
/// when `hotkey_handle` is Some, which can only happen if an earlier
/// [`try_install`] succeeded, which is impossible on stock builds. Kept
/// so the call site type-checks without a `#[cfg]` guard.
#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
pub(crate) fn resume_key_names_from_env() -> std::result::Result<Vec<String>, String> {
    Err("in-process resume unavailable on stock build".to_owned())
}

/// Split the PTT `settings.key` string into individual key names.
/// Mirrors [`super::dictate_run::split_key_names`] byte-for-byte;
/// duplicated (not re-exported) so this module compiles cleanly when
/// `rust-hotkeys+rust-injection` are gated off (the whole in_process
/// runtime module compiles on stock builds so its `try_install` stub
/// can be called from the supervisor unconditionally).
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
fn split_key_names(chord: &str) -> Vec<String> {
    chord
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

// ── worker-command env application ───────────────────────────────────────────

/// Env-var key prefix the in-process runtime cares about — the
/// `VOICEPI_*` entries in [`WorkerCommand::env`] are the
/// config-derived settings the real Rust backends read from process
/// env. Child-only knobs (`PYTHONPATH`, `VOICEPI_RUST_INJECTOR`) are
/// filtered out so the runtime's process env surface stays small.
const IN_PROCESS_ENV_PREFIX: &str = "VOICEPI_";

/// F1 (Codex P1 PR #519 supervisor.rs:467): apply the
/// [`WorkerCommand`]'s `VOICEPI_*` env vector to the process
/// environment so the in-process backends see the same view a Python
/// child would inherit through `.envs()`. Without this, saved schema
/// settings (language, initial prompt, audio device, inject mode,
/// recording thresholds, ...) that the UI wrote via `worker_command()`
/// are silently discarded when the supervisor takes the Phase B path,
/// and the real backends fall back to defaults.
///
/// Semantics match `Command::envs()`: command values clobber any
/// pre-existing process env entry, mirroring what the Python child
/// would see. One-shot mutation, same pattern as
/// [`super::rust_session_sink::build_production_sink`]'s
/// `WORKER_EVENTS_ENV` set — the supervisor is single-threaded with
/// respect to its own setup and this is called at most once per
/// process lifetime (the `hotkey_handle` slot short-circuits
/// subsequent starts), so no `ENV_LOCK` is needed.
pub(crate) fn apply_worker_command_env(command: &WorkerCommand) {
    for (key, value) in command.env.iter() {
        if !key.starts_with(IN_PROCESS_ENV_PREFIX) {
            continue;
        }
        // Skip child-only knobs (`VOICEPI_RUST_INJECTOR` is the
        // Python child's shell-back pointer to `whisper-dictate
        // inject`; in-process injects directly through enigo) and
        // the engine env var we already resolved in
        // `engine_choice_from_env` (re-applying would be a no-op
        // but skip so a test seeding the var deliberately after
        // resolution is not clobbered by an in-vector duplicate).
        if key == "VOICEPI_RUST_INJECTOR" || key == ENGINE_ENV {
            continue;
        }
        std::env::set_var(key, value);
    }
}

// Unit tests moved to sibling `in_process_tests.rs` (Codex P2 PR
// #519 in_process.rs:444) so the production module stays under the
// AGENTS.md 500-LOC modularity limit.
