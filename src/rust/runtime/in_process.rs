//! In-process Rust dictation dispatch: Phase B of audit item 5.
//!
//! When the operator opts in with `VOICEPI_DICTATE_ENGINE=rust`, the
//! [`super::supervisor::RuntimeSupervisor::start`] entry point installs
//! the full Rust dictation runtime (hotkey listener + coordinator +
//! session sink + real backends when the required features are compiled
//! in) inside the UI process itself, instead of spawning a Python worker
//! child. This removes the Python-side dispatcher that Phase A
//! introduced (`whisper-dictate dictate-run` subprocess) from the
//! runtime supervision ladder.
//!
//! See `docs/design/item5-phase-b-inprocess.md` for the design and the
//! five risks (config-parsing drift, status-event parity, panic
//! containment, model-load UX, env-var nomenclature) this module
//! addresses.
//!
//! ## Env-var contract
//!
//! * [`ENGINE_ENV`] (`VOICEPI_DICTATE_ENGINE`) — canonical Phase A/B
//!   switch. Values: [`ENGINE_PYTHON`] (default), [`ENGINE_RUST`]
//!   (in-process from Phase B onward). Case-insensitive; blank / unknown
//!   values fall through to the Python path with a stderr warning.
//! * `VOICEPI_DICTATE_BACKEND=rust-session` — older, lower-level opt-in
//!   at
//!   [`super::rust_session_sink::dictate_backend_rust_session_requested`].
//!   When set alongside `VOICEPI_DICTATE_ENGINE=rust`, the ENGINE flag
//!   wins (Phase B design doc risk #5): the supervisor treats
//!   `ENGINE=rust` as the authoritative signal and skips the Python
//!   worker. An informational stderr line names the effective backend
//!   so an operator with both variables set sees which one won.
//!
//! ## Failure model
//!
//! * **Feature-gated.** Requires `rust-hotkeys` + `rust-injection`. On a
//!   stock build the install refuses with an actionable rebuild message
//!   and the supervisor falls back to the Python subprocess path.
//! * **Panic containment.** [`try_install`] wraps the install in
//!   [`std::panic::catch_unwind`] and converts any panic into an
//!   [`InProcessInstallError::Panicked`], from which the supervisor's
//!   caller falls back to the Python worker path. Panics AFTER install
//!   (on the coordinator / manager threads) still abort the process —
//!   that scope is limited to what the design doc calls "install
//!   boundary".
//! * **Auto-fallback.** Any [`InProcessInstallError`] is a fallback
//!   signal: the supervisor logs a stderr line naming the reason and
//!   spawns the Python worker with `VOICEPI_DICTATE_ENGINE` cleared for
//!   the child so it does not attempt to re-enter Phase A's
//!   subprocess-of-a-subprocess pipeline.

use std::sync::mpsc::Sender;

use super::supervisor::{RuntimeEvent, WorkerEvent};

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
            Self::HotkeyInstallFailed(msg) => write!(
                f,
                "in-process Rust hotkey install failed ({msg}); \
                 falling back to the Python worker"
            ),
            Self::Panicked(msg) => write!(
                f,
                "in-process Rust runtime install panicked ({msg}); \
                 falling back to the Python worker. This is a bug — please file \
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

    // 2. Build the same production session sink `dictate-run` builds.
    //    Reuses `rust_session_sink::build_production_sink` so the
    //    worker-event stream the UI observes is identical whether the
    //    engine ran under the CLI verb (Phase A) or in-process (Phase B).
    let (sink, coord_slot) =
        super::rust_session_sink::build_production_sink(tx.clone(), repaint_notifier);

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

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn engine_choice_unset_is_python() {
        assert_eq!(EngineChoice::from_env_value(None), EngineChoice::Python);
    }

    #[test]
    fn engine_choice_blank_is_python() {
        assert_eq!(EngineChoice::from_env_value(Some("")), EngineChoice::Python);
        assert_eq!(
            EngineChoice::from_env_value(Some("   ")),
            EngineChoice::Python
        );
    }

    #[test]
    fn engine_choice_explicit_python() {
        assert_eq!(
            EngineChoice::from_env_value(Some("python")),
            EngineChoice::Python
        );
        // Case-insensitive so `PYTHON`, `Python` and stray whitespace
        // all resolve to the same canonical variant.
        assert_eq!(
            EngineChoice::from_env_value(Some(" Python ")),
            EngineChoice::Python
        );
    }

    #[test]
    fn engine_choice_rust() {
        assert_eq!(
            EngineChoice::from_env_value(Some("rust")),
            EngineChoice::Rust
        );
        assert_eq!(
            EngineChoice::from_env_value(Some("RUST")),
            EngineChoice::Rust
        );
        assert_eq!(
            EngineChoice::from_env_value(Some(" rust ")),
            EngineChoice::Rust
        );
    }

    #[test]
    fn engine_choice_unknown_carries_raw_value() {
        match EngineChoice::from_env_value(Some("go")) {
            EngineChoice::Unknown(raw) => assert_eq!(raw, "go"),
            other => panic!("expected Unknown(\"go\"), got {other:?}"),
        }
    }

    #[test]
    fn features_available_matches_cfg() {
        assert_eq!(
            features_available(),
            cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
        );
    }

    #[test]
    fn ready_worker_event_shape_matches_python_ready() {
        // Contract with the UI: emit a WorkerEvent whose `event="status"`
        // and `state=Some("ready")` so `worker_ready_for_state("ready")`
        // fires the same latch the Python worker triggers. Regression
        // test for design doc risk #2.
        let (tx, rx) = mpsc::channel();
        emit_ready_worker_event(&tx);
        let received = rx.try_recv().expect("ready worker event enqueued");
        match received {
            RuntimeEvent::Worker(worker) => {
                assert_eq!(worker.event, "status");
                assert_eq!(worker.state.as_deref(), Some("ready"));
                // The `engine` field is Phase B-specific so operators
                // can tell an in-process ready apart from a Python one.
                assert_eq!(
                    worker.payload.get("engine").and_then(|v| v.as_str()),
                    Some("rust"),
                );
            }
            other => panic!("expected RuntimeEvent::Worker, got {other:?}"),
        }
    }

    #[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
    #[test]
    fn try_install_stock_build_returns_features_missing() {
        // On a stock build the supervisor's Phase B branch MUST fail
        // fast with an actionable message so the caller can fall back
        // to the Python worker without spinning up any threads. This
        // pins the contract the fallback path relies on.
        let (tx, _rx) = mpsc::channel();
        let result = try_install(tx, None);
        assert!(
            matches!(result, Err(InProcessInstallError::FeaturesMissing)),
            "stock build must refuse in-process install with FeaturesMissing",
        );
        let err = result
            .err()
            .expect("stock build must refuse in-process install");
        let msg = err.to_string();
        assert!(
            msg.contains("rust-hotkeys") && msg.contains("rust-injection"),
            "error must name the missing features: {msg}"
        );
        assert!(
            msg.contains("cargo build --features"),
            "error must include the rebuild command: {msg}"
        );
    }

    #[test]
    fn catch_unwind_panic_string_literal_lands_as_panicked_error() {
        // Design doc risk #3: a panic inside the install path must
        // convert into a recoverable InProcessInstallError::Panicked
        // rather than aborting the UI process. This pins the
        // stringifier that runs on the recovery path so a future
        // refactor that swaps `catch_unwind` for something else is
        // caught by a test failure. Feature-independent because the
        // stringifier itself is pure.
        let payload = std::panic::catch_unwind(|| panic!("boom-from-test"))
            .expect_err("literal panic must land in catch_unwind Err arm");
        let msg = stringify_panic(payload);
        assert!(msg.contains("boom-from-test"), "stringifier lost the payload: {msg}");
        // And the same round-trips for owned-String payloads (which is
        // what `assert!(false, "…")` produces internally).
        let payload = std::panic::catch_unwind(|| panic!("owned {}", "message"))
            .expect_err("formatted panic must land in Err");
        let msg = stringify_panic(payload);
        assert!(msg.contains("owned message"), "stringifier lost owned payload: {msg}");
    }

    #[test]
    fn env_precedence_note_fires_only_when_both_env_vars_set() {
        // Design doc risk #5: with BOTH `VOICEPI_DICTATE_ENGINE=rust`
        // AND `VOICEPI_DICTATE_BACKEND=rust-session` set, the
        // supervisor emits an informational line naming the effective
        // backend. With only ENGINE=rust set, no line fires.
        //
        // Uses a process-wide mutex because `std::env::set_var` is not
        // thread-safe on Unix.
        use std::sync::{Mutex, OnceLock};
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let previous = std::env::var("VOICEPI_DICTATE_BACKEND").ok();

        // Case 1: backend unset — no line.
        std::env::remove_var("VOICEPI_DICTATE_BACKEND");
        let (tx, rx) = mpsc::channel();
        maybe_emit_env_precedence_note(&tx);
        assert!(rx.try_recv().is_err(), "no line without rust-session set");

        // Case 2: backend set to rust-session — informational line
        // fires naming both env vars.
        std::env::set_var("VOICEPI_DICTATE_BACKEND", "rust-session");
        let (tx, rx) = mpsc::channel();
        maybe_emit_env_precedence_note(&tx);
        match rx.try_recv().expect("precedence note enqueued") {
            RuntimeEvent::Stderr(line) => {
                assert!(line.contains("VOICEPI_DICTATE_ENGINE"), "line names ENGINE: {line}");
                assert!(line.contains("VOICEPI_DICTATE_BACKEND"), "line names BACKEND: {line}");
                assert!(line.contains("wins"), "line names the precedence: {line}");
            }
            other => panic!("expected Stderr, got {other:?}"),
        }

        // Restore.
        match previous {
            Some(v) => std::env::set_var("VOICEPI_DICTATE_BACKEND", v),
            None => std::env::remove_var("VOICEPI_DICTATE_BACKEND"),
        }
    }

    #[test]
    fn install_error_display_covers_every_variant() {
        // Sonar-friendly: every user-facing error variant must have a
        // non-empty Display impl so the supervisor's stderr forwarding
        // has something to log. Missing a variant here is a refactor
        // regression signal.
        assert!(!InProcessInstallError::FeaturesMissing
            .to_string()
            .is_empty());
        assert!(!InProcessInstallError::ConfigLoadFailed("boom".to_owned())
            .to_string()
            .is_empty());
        assert!(!InProcessInstallError::EmptyChord.to_string().is_empty());
        assert!(
            !InProcessInstallError::HotkeyInstallFailed("nope".to_owned())
                .to_string()
                .is_empty()
        );
        assert!(!InProcessInstallError::Panicked("crash".to_owned())
            .to_string()
            .is_empty());
    }
}
