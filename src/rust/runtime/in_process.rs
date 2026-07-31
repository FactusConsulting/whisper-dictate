//! In-process Rust dictation dispatch.
//!
//! [`super::supervisor::RuntimeSupervisor::start`] installs
//! the full Rust dictation runtime (hotkey listener + coordinator +
//! session sink + real backends when the required features are
//! compiled in) inside the UI process itself, instead of spawning a
//! a second runtime process.
//!
//! See `docs/design/item5-phase-b-inprocess.md` for the design and the
//! five risks (config-parsing drift, status-event parity, panic
//! containment, model-load UX, env-var nomenclature) this module
//! addresses.
//!
//! ## Env-var contract
//!
//! * [`ENGINE_ENV`] (`VOICEPI_DICTATE_ENGINE`) is retained only for
//!   migration diagnostics. Blank, unset, and `rust` select this runtime;
//!   `python` and unknown values are rejected by the caller.
//! * `VOICEPI_DICTATE_BACKEND=rust-session` — older lower-level opt-in.
//!   When set alongside `VOICEPI_DICTATE_ENGINE=rust`, ENGINE wins
//!   (design doc risk #5) and an informational stderr line names the
//!   effective backend.
//!
//! ## Failure model
//!
//! Any [`InProcessInstallError`] is surfaced to the UI and persistent
//! diagnostic log. Feature-gated on both
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

/// Retained engine selector name used for migration diagnostics.
pub(crate) const ENGINE_ENV: &str = "VOICEPI_DICTATE_ENGINE";

/// Ambient values replaced by the current native session. A setting cleared
/// in the UI is absent from the next WorkerCommand, so every applied VOICEPI
/// value—not just credentials—must be restored before that command is built.
fn session_env_originals(
) -> &'static std::sync::Mutex<std::collections::BTreeMap<String, Option<std::ffi::OsString>>> {
    static ORIGINALS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, Option<std::ffi::OsString>>>,
    > = std::sync::OnceLock::new();
    ORIGINALS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
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

/// Reason [`try_install`] refused. Each variant maps to an actionable
/// runtime error and diagnostic stage.
#[derive(Debug)]
#[allow(dead_code)] // Panicked / HotkeyInstallFailed only construct on feature builds
pub(crate) enum InProcessInstallError {
    /// Build was not compiled with `rust-hotkeys` + `rust-injection`.
    /// Actionable message names the rebuild command.
    FeaturesMissing,
    /// Config load failed (`config::load_settings`). Wraps the anyhow
    /// error string; the supervisor forwards it verbatim to stderr.
    ConfigLoadFailed(String),
    /// Effective runtime options request a capability absent from this
    /// build. Checked after applying saved/UI overrides and before any
    /// audio, model, coordinator, or hotkey resources are installed.
    InvalidOptions(String),
    /// Config's PTT `settings.key` was empty. Same message shape as the
    /// `dictate-run` verb so users get consistent guidance.
    EmptyChord,
    /// Real Rust backend refused: missing feature, cpal device
    /// unavailable, model resolution failed, Silero ONNX missing.
    /// Wraps the reason from `try_build_production_sink`.
    MissingBackend(String),
    /// [`crate::hotkey::install_hotkey`] failed. Wraps the underlying
    /// [`crate::hotkey::InstallError`] message; keeps the supervisor
    /// independent of the concrete error variants (they may grow).
    HotkeyInstallFailed(String),
    /// Another whisper-dictate process already owns push-to-talk
    /// ([`crate::hotkey::ptt_lock`]).
    ///
    /// Split out of [`Self::HotkeyInstallFailed`] deliberately, even
    /// though it is one more variant for the supervisor to match on.
    /// It keeps ownership conflicts distinct from device/config failures
    /// and carries the full refusal text naming the holding pid.
    PttAlreadyHeld(String),
    /// [`std::panic::catch_unwind`] caught a panic during install.
    /// Payload is a best-effort stringification of the panic message.
    Panicked(String),
}

impl std::fmt::Display for InProcessInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FeaturesMissing => write!(
                f,
                "in-process Rust runtime needs `rust-hotkeys`, `rust-injection`, \
                 `audio-in-rust`, and `whisper-rs-local` (rebuild with \
                 `cargo build --features \
                 rust-hotkeys,rust-injection,audio-in-rust,whisper-rs-local`)"
            ),
            Self::ConfigLoadFailed(msg) => {
                write!(f, "in-process Rust runtime could not load config ({msg})")
            }
            Self::InvalidOptions(msg) => {
                write!(
                    f,
                    "in-process Rust runtime rejected its effective options ({msg})"
                )
            }
            Self::EmptyChord => write!(
                f,
                "in-process Rust runtime refused: no PTT chord configured \
                 (settings.key is empty); set one via \
                 `whisper-dictate config set key ctrl_l+shift_l` and retry"
            ),
            Self::MissingBackend(msg) => write!(
                f,
                "in-process Rust runtime cannot serve PTT ({msg}). Rebuild with the \
                 `whisper-rs-local`, `rust-injection`, and `audio-in-rust` \
                 cargo features and download a Whisper model to enable \
                 the in-process path"
            ),
            Self::HotkeyInstallFailed(msg) => {
                write!(f, "in-process Rust hotkey install failed ({msg})")
            }
            Self::PttAlreadyHeld(msg) => {
                write!(f, "{msg} This process will not respond to the chord.")
            }
            Self::Panicked(msg) => write!(
                f,
                "in-process Rust runtime install panicked ({msg}). This is a bug - please file \
                 an issue at https://github.com/lars-frost/whisper-dictate/issues"
            ),
        }
    }
}

// ── worker-event helpers ─────────────────────────────────────────────────────

/// Emit the established `worker_ready` status event on model-load completion,
/// so the UI's ready latch fires for the native runtime. Runs on the supervisor's
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

/// Feature-gated install: on a reduced build this immediately returns
/// [`InProcessInstallError::FeaturesMissing`]; on a feature-complete build it
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
/// `RuntimeSupervisor::hotkey_handle` so the coordinator threads survive
/// across restart calls.
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(crate) fn try_install(
    tx: Sender<RuntimeEvent>,
    repaint_notifier: Option<super::supervisor::RepaintNotifier>,
    ambient_live_env: std::collections::BTreeMap<String, String>,
) -> std::result::Result<InProcessInstallation, InProcessInstallError> {
    // Panic containment (design doc risk #3). AssertUnwindSafe is
    // required because `Sender<RuntimeEvent>` is not by default UnwindSafe
    // — the supervisor owns its own clone and any partial `send` before
    // the panic is a no-op the receiver will ignore.
    let install_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        install_supported(tx.clone(), repaint_notifier, ambient_live_env)
    }));
    match install_result {
        Ok(Ok(installation)) => Ok(installation),
        Ok(Err(err)) => Err(err),
        Err(panic_payload) => Err(InProcessInstallError::Panicked(stringify_panic(
            panic_payload,
        ))),
    }
}

/// Reduced-build stub — always returns [`InProcessInstallError::FeaturesMissing`]
/// without spinning up any threads. The `tx` / `repaint_notifier` args are consumed
/// as `_` so the call shape stays identical across feature configurations.
#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
pub(crate) fn try_install(
    _tx: Sender<RuntimeEvent>,
    _repaint_notifier: Option<super::supervisor::RepaintNotifier>,
    _ambient_live_env: std::collections::BTreeMap<String, String>,
) -> std::result::Result<InProcessInstallation, InProcessInstallError> {
    Err(InProcessInstallError::FeaturesMissing)
}

/// Map a [`crate::hotkey::InstallError`] onto the supervisor-facing
/// [`InProcessInstallError`].
///
/// Two variants get their own identity and everything else collapses
/// into [`InProcessInstallError::HotkeyInstallFailed`]:
///
/// * `Unsupported` — a build-configuration problem, not a runtime one.
/// * `AlreadyHeld` — identifies an ownership conflict without putting a
///   second listener on the same chord.
///
/// A named function rather than an inline closure so the classification
/// is unit-testable without a live install — the in-process path builds a
/// real capture + transcription session before it ever reaches
/// `install_hotkey`, so there is no way to drive this mapping end to end
/// on a headless runner.
///
/// Always compiled so its test runs on every CI leg; on stock builds
/// nothing but the test calls it.
#[allow(dead_code)]
pub(crate) fn classify_hotkey_install_error(
    err: crate::hotkey::InstallError,
) -> InProcessInstallError {
    use crate::hotkey::InstallError;
    match err {
        InstallError::Unsupported => InProcessInstallError::FeaturesMissing,
        err @ InstallError::AlreadyHeld { .. } => {
            InProcessInstallError::PttAlreadyHeld(err.to_string())
        }
        other => InProcessInstallError::HotkeyInstallFailed(other.to_string()),
    }
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

/// Live in-process installation. The supervisor drops this installation on
/// Stop and rebuilds it on Start/Restart so audio, STT, injection and hotkey
/// configuration all follow the newly loaded settings.
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
    /// Shared with the production injection backend. The supervisor flips
    /// this before dropping the listener so in-flight transcription cannot
    /// inject after Stop has completed.
    pub(crate) runtime_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Closes the microphone before asynchronous coordinator teardown waits
    /// for any in-flight transcription.
    pub(crate) capture_stop: super::supervisor::CaptureStop,
    /// PTT key names the hotkey manager was actually registered with.
    /// The supervisor's `in_process_install_summary` uses this instead
    /// of a fresh environment read so a settings save
    /// racing the install cannot log a chord that differs from the
    /// one the listener is bound to (Codex P2 #644 r3659201761).
    pub(crate) key_names: Vec<String>,
    /// Kept alive so the session sink's `on_processing_finished`
    /// callback survives; the callback captures a clone of the same
    /// `Arc<OnceLock<_>>` and reads the slot every stop.
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
    /// Never populated on the stock build (the constructor is not
    /// reached), but declared so the supervisor's Phase-B call site
    /// compiles with the same field name across feature configs.
    pub(crate) key_names: Vec<String>,
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
    ambient_live_env: std::collections::BTreeMap<String, String>,
) -> std::result::Result<InProcessInstallation, InProcessInstallError> {
    use crate::config::load_settings;
    use crate::hotkey::{coordinator, install_hotkey, HotkeyConfig};

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
    // Capture the exact names that will be handed to `install_hotkey`
    // so `InProcessInstallation.key_names` records the chord the
    // listener is actually bound to. The supervisor's Phase-B "started"
    // line reads THIS instead of re-loading settings, closing the race
    // window a second read would open (Codex P2 #644 r3659201761).
    let installed_key_names = key_names.clone();
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
    let (sink, coord_slot, runtime_active, capture_stop) =
        super::rust_session_sink::try_build_production_sink(
            tx.clone(),
            repaint_notifier,
            super::live_settings::LiveEnvOverrides {
                ambient: ambient_live_env,
                forced: std::collections::BTreeMap::new(),
            },
        )
        .map_err(InProcessInstallError::MissingBackend)?;

    // 3. Install the hotkey with the sink as the action target. Wraps
    //    `install_hotkey`'s per-error variants into a single
    //    fallback-eligible `HotkeyInstallFailed` so the supervisor's
    //    caller does not need to know the hotkey error taxonomy.
    let handle = install_hotkey(
        HotkeyConfig {
            key_names,
            mode,
            auto_complete_processing: false,
        },
        sink,
    )
    .map_err(classify_hotkey_install_error)?;

    // 4. Wire the coordinator handle back into the sink's OnceLock so
    //    `on_processing_finished` can send `ProcessingFinished(id)` when
    //    a stop completes — otherwise the coordinator stays parked in
    //    `Stage::Processing` and the next press is ignored. Same shape
    //    A duplicate-set is a refactor regression signal, not fatal.
    if coord_slot.set(handle.coordinator_handle()).is_err() {
        let _ = tx.send(RuntimeEvent::Stderr(
            "[in-process] coordinator handle slot already populated; \
             ignoring (this indicates a refactor regression but is not fatal)"
                .to_owned(),
        ));
    }

    Ok(InProcessInstallation {
        hotkey_handle: handle,
        runtime_active,
        capture_stop,
        key_names: installed_key_names,
        coord_slot_keepalive: coord_slot,
    })
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
/// respect to its own setup. Restart restores session-scoped credentials
/// before applying the replacement command.
pub(crate) fn apply_worker_command_env(command: &WorkerCommand) {
    restore_session_scoped_env();

    let mut session_originals = session_env_originals()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
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
        session_originals
            .entry(key.clone())
            .or_insert_with(|| std::env::var_os(key));
        std::env::set_var(key, value);
        if crate::diag::trace_enabled() {
            crate::diag::log!("[runtime/trace] applied session env key={key}");
        }
    }
    drop(session_originals);
    crate::diag::init_from_env();
}

/// Restore every value written by the prior native session before the UI
/// constructs a replacement WorkerCommand. Otherwise absent/cleared settings
/// fall back to stale process environment and credential provenance checks
/// misclassify session-written secrets as caller-owned.
pub(crate) fn restore_session_scoped_env() {
    let mut session_originals = session_env_originals()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    for (key, original) in std::mem::take(&mut *session_originals) {
        match original {
            Some(value) => std::env::set_var(&key, value),
            None => std::env::remove_var(&key),
        }
        if crate::diag::trace_enabled() {
            crate::diag::log!("[runtime/trace] restored ambient session env key={key}");
        }
    }
}

// Unit tests moved to sibling `in_process_tests.rs` (Codex P2 PR
// #519 in_process.rs:444) so the production module stays under the
// AGENTS.md 500-LOC modularity limit.
