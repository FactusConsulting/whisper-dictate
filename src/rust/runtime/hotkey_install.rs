//! Rust-hotkey install / restart wiring for the runtime supervisor.
//!
//! Owns the pure decision helpers ([`parse_toggle_value`],
//! [`extract_hotkey_key_names`], [`restart_hotkey_decision`],
//! [`normalise_hotkey_chord_for_python`]) plus the two sink flavours
//! ([`install_logger_sink_hotkey`], [`install_session_sink_hotkey`])
//! that the supervisor's `start()` path calls into when the user opted
//! into the Rust hotkey backend via `VOICEPI_HOTKEY_BACKEND=rust`.
//!
//! Split out of `runtime.rs` in the 500-LOC modularity refactor so the
//! hotkey-install logic is co-located and unit-testable.

use super::rust_session_sink;
use super::{RepaintNotifier, RuntimeEvent, WorkerCommand};

/// Parse a `VOICEPI_TOGGLE` env-var value to a boolean. Accepts the same
/// set of truthy strings that the Python config layer and shell tooling use:
/// `"true"`, `"1"`, `"yes"`, `"on"` — all case-insensitive. Returns `false`
/// for any other value, including empty string.
///
/// Extracted as a standalone helper so it can be unit-tested independently
/// of the full command-build pipeline (Codex P2 finding, PR #373).
pub(crate) fn parse_toggle_value(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// Normalise rdev-specific PTT-key aliases in the worker command's
/// `VOICEPI_KEY` to their canonical pynput-compatible names so the Python
/// listener doesn't terminate the worker at startup when it tries to
/// resolve them against `pynput.keyboard.Key.<name>`.
///
/// Today this maps `right_alt` and `ralt` → `alt_gr` (the canonical
/// right-Alt name pynput knows). The Rust hotkey backend accepts both
/// aliases for user-facing convenience (P2 #346 finding 4); without this
/// post-processing a user that configured `right_alt` would crash the
/// Python worker even when the Rust listener took over the hotkey
/// lifecycle, because pynput resolves the keyname at startup BEFORE the
/// listener registers (so `VOICEPI_PYTHON_HOTKEY=0` does not save us).
///
/// Applied to every `+`-separated segment so chord bindings like
/// `ctrl_r+right_alt` work too. P3 #383.
pub(crate) fn normalise_hotkey_aliases_for_python(env: &mut [(String, String)]) {
    for (key, value) in env.iter_mut() {
        if key == "VOICEPI_KEY" {
            *value = normalise_hotkey_chord_for_python(value);
        }
    }
}

/// Pure helper for [`normalise_hotkey_aliases_for_python`] — split out so
/// the alias transformation can be unit-tested without constructing a
/// full WorkerCommand env vector. Preserves the input's `+` separators
/// and per-segment formatting; only the matched aliases are rewritten.
pub(crate) fn normalise_hotkey_chord_for_python(raw: &str) -> String {
    raw.split('+')
        .map(
            |segment| match segment.trim().to_ascii_lowercase().as_str() {
                "right_alt" | "ralt" => "alt_gr".to_owned(),
                _ => segment.to_owned(),
            },
        )
        .collect::<Vec<_>>()
        .join("+")
}

/// Extract PTT key names from a [`WorkerCommand`]'s environment: split
/// `VOICEPI_KEY` on `+`, trim whitespace, drop empty segments. Returns an
/// empty `Vec` when the env var is absent or blank — callers use this as a
/// "no hotkey configured" sentinel.
pub(crate) fn extract_hotkey_key_names(command: &WorkerCommand) -> Vec<String> {
    command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_KEY")
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Set the `VOICEPI_PYTHON_HOTKEY=0` env var on `command` so the spawned
/// Python worker's `KeyBackendMixin.run` parks itself instead of installing
/// pynput/evdev. The supervisor MUST only call this after it has
/// successfully installed the Rust hotkey subsystem
/// ([`maybe_install_rust_hotkey`] returned `Ok`) — otherwise neither
/// backend will be listening and PTT will be broken for the entire session.
///
/// Idempotent: if the flag is already present in `command.env`, the value
/// is replaced.
///
/// **Known limitation (PR #344 P2 #7).** The Rust backend only owns the
/// PTT chord; the user-configurable multi-press quit key
/// (`VOICEPI_QUIT_KEY` / `VOICEPI_QUIT_COUNT`, default 3× Esc) lives in
/// the Python listener that this flag parks. When the Rust backend is
/// active, users can still quit via Ctrl+C from the terminal and via the
/// UI tray menu, but the configured multi-press hotkey is inactive. A
/// follow-up will carry the quit binding into the Rust path; the warning
/// emitted at install time documents the current state.
pub fn disable_python_hotkey(command: &mut WorkerCommand) {
    const KEY: &str = "VOICEPI_PYTHON_HOTKEY";
    if let Some(slot) = command.env.iter_mut().find(|(k, _)| k == KEY) {
        slot.1 = "0".to_owned();
        return;
    }
    command.env.push((KEY.to_owned(), "0".to_owned()));
}

/// Outcome of the restart-path key-binding decision (see
/// [`restart_hotkey_decision`]). Mapped 1:1 onto the supervisor's
/// restart branch in [`super::supervisor::RuntimeSupervisor::start`] (`else if let
/// Some(handle) = self.hotkey_handle.as_ref()`):
///
/// * [`SkipNoKey`](Self::SkipNoKey) — `VOICEPI_KEY` is unset or blank;
///   nothing to resume. The previous successful install handled the
///   Python-park gate; this branch must leave it alone.
/// * [`SkipUnsupported`](Self::SkipUnsupported) — the configured key
///   is not in the Rust (rdev) supported list. The supervisor MUST
///   NOT park Python on this branch — otherwise PTT goes silent for
///   the whole session, the regression Codex P2 PR #421
///   runtime.rs:530 guards against.
/// * [`Resume`](Self::Resume) — the key is supported; the supervisor
///   calls `handle.resume(key_names)`. `park_python` is true only
///   when [`rust_session_sink::dictate_backend_rust_session_requested`]
///   is true (the in-process session sink owns the lifecycle).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RestartHotkeyDecision {
    SkipNoKey,
    SkipUnsupported {
        key_names: Vec<String>,
    },
    Resume {
        key_names: Vec<String>,
        park_python: bool,
    },
}

/// Pure decision helper for the supervisor's restart-path branch.
/// Extracted so the gate is unit-testable WITHOUT populating a live
/// [`crate::hotkey::HotkeyHandle`] (which requires the `rust-hotkeys`
/// feature plus a working rdev install — neither available in
/// headless CI).
///
/// Codex P2 PR #421 runtime.rs:530 — the matching unit test in
/// `hotkey_supervisor_tests` asserts the three branches of this
/// helper so a future edit that flipped the gate (e.g. parking
/// Python BEFORE validating) would have to fail a test before it
/// could land.
pub(crate) fn restart_hotkey_decision(
    command: &WorkerCommand,
    rust_session_requested: bool,
) -> RestartHotkeyDecision {
    let key_names = extract_hotkey_key_names(command);
    if key_names.is_empty() {
        return RestartHotkeyDecision::SkipNoKey;
    }
    if crate::hotkey::validate_key_names(&key_names).is_err() {
        return RestartHotkeyDecision::SkipUnsupported { key_names };
    }
    RestartHotkeyDecision::Resume {
        key_names,
        park_python: rust_session_requested,
    }
}

/// Whether the user requested the Rust-side hotkey backend via
/// `VOICEPI_HOTKEY_BACKEND=rust`. Pure helper so the gate is unit-testable.
/// Delegates to [`crate::hotkey::rust_hotkey_backend_requested`].
pub fn rust_hotkey_backend_requested() -> bool {
    crate::hotkey::rust_hotkey_backend_requested()
}

/// True when the user requested the Rust hotkey backend AND the binary was
/// built with the `rust-hotkeys` feature. The supervisor uses this to decide
/// whether to install the Rust hotkey subsystem and silence the Python
/// listener; both must be true, otherwise we stay on pynput (and log a
/// one-line warning if only the env var is set — handled at install time).
pub fn rust_hotkey_backend_active() -> bool {
    crate::hotkey::rust_hotkey_backend_requested() && crate::hotkey::rust_hotkey_backend_available()
}

/// Install the Rust hotkey subsystem at supervisor startup, if requested.
///
/// Returns `Some(handle)` ONLY when the env var was set, the feature is
/// compiled in, AND every layer (target validation, listener startup,
/// register) succeeded. Returns `None` (and logs a one-line warning when
/// there's something to warn about) in every other case.
///
/// **Python is NOT disabled** even on a successful install. The coordinator
/// today only logs coordinator actions ([`crate::hotkey::coordinator::CoordinatorAction`]
/// as `[hotkey]` lines); no IPC yet drives the worker's actual recording
/// lifecycle. Disabling Python before that IPC is wired would leave PTT
/// completely silent. Follow-up work will wire the IPC and then add the
/// disable step. Until then both backends run side-by-side (PR #373 Fix 1).
///
/// `action_sink` is invoked on the coordinator thread for every
/// [`crate::hotkey::coordinator::CoordinatorAction`] the state machine
/// emits; the caller is responsible for translating those into worker
/// start / stop commands (today: logging only — see above).
///
/// The caller MUST send
/// [`crate::hotkey::coordinator::CoordinatorEvent::ProcessingFinished`]
/// (with the matching recording id) via
/// [`crate::hotkey::HotkeyHandle::processing_finished`] when the worker
/// finishes transcription — otherwise the coordinator stays parked in
/// [`crate::hotkey::coordinator::Stage::Processing`] and ignores the next
/// press.
pub fn maybe_install_rust_hotkey<F>(
    key_names: Vec<String>,
    mode: crate::hotkey::coordinator::Mode,
    action_sink: F,
) -> Option<crate::hotkey::HotkeyHandle>
where
    F: FnMut(crate::hotkey::coordinator::CoordinatorAction) + Send + 'static,
{
    if !crate::hotkey::rust_hotkey_backend_requested() {
        return None;
    }
    if !crate::hotkey::rust_hotkey_backend_available() {
        eprintln!(
            "[hotkey] VOICEPI_HOTKEY_BACKEND=rust was set but this binary was \
             built without --features rust-hotkeys; falling back to the Python \
             listener (pynput). Rebuild with the feature to use the Rust backend."
        );
        return None;
    }
    let cfg = crate::hotkey::HotkeyConfig { key_names, mode };
    match crate::hotkey::install_hotkey(cfg, action_sink) {
        Ok(handle) => {
            // P2 #7: the Rust backend only owns the PTT chord; the
            // configured multi-press quit key (VOICEPI_QUIT_KEY /
            // VOICEPI_QUIT_COUNT, default 3× Esc) lives in the Python
            // listener that the supervisor is about to park. Warn so
            // users who rely on the hotkey quit know it's inactive.
            eprintln!(
                "[hotkey] Rust backend active; the configured multi-press \
                 quit key (VOICEPI_QUIT_KEY / VOICEPI_QUIT_COUNT) is \
                 currently only honoured by the Python listener. Quit \
                 via Ctrl+C in the terminal or the tray menu."
            );
            Some(handle)
        }
        Err(err) => {
            eprintln!(
                "[hotkey] Rust hotkey backend install failed: {err}; \
                 falling back to the Python listener (pynput)."
            );
            None
        }
    }
}

/// Extract the PTT key names and toggle mode from a [`WorkerCommand`]'s
/// environment, then call [`maybe_install_rust_hotkey`] with one of two
/// action sinks:
///
/// * **Default (production today):** a logger sink that turns each
///   [`crate::hotkey::coordinator::CoordinatorAction`] into a
///   `[hotkey]`-prefixed `RuntimeEvent::Stderr` line. The Python worker
///   still owns the live recording lifecycle.
/// * **`VOICEPI_DICTATE_BACKEND=rust-session`** (Wave 5 PR 4 of #348):
///   a session-backed sink that drives an in-process
///   [`crate::dictate::DictateSession`] (start / stop_and_transcribe /
///   cancel) and signals
///   [`crate::hotkey::coordinator::CoordinatorEvent::ProcessingFinished`]
///   back into the coordinator when the stop completes. Uses stub
///   `TranscribeBackend` / `InjectBackend` impls (PR 5 swaps them for
///   `LocalWhisper` + the real injection dispatcher). Frames stay
///   unwired until PR 3 (#415) + PR 5 land; the env-var gate keeps
///   production on the logger sink so PR 4 is observable but inert.
///
/// Returns the live handle on success so the caller can call
/// [`disable_python_hotkey`]; returns `None` when the backend is not
/// requested, not available, or the key config is missing / empty.
///
/// P2 #346 finding 1 (logger sink) + Wave 5 PR 4 (session sink).
pub(crate) fn install_rust_hotkey_from_command(
    command: &WorkerCommand,
    tx: std::sync::mpsc::Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> Option<crate::hotkey::HotkeyHandle> {
    let key_names = extract_hotkey_key_names(command);
    if key_names.is_empty() {
        return None;
    }
    // P2 #373 finding 2: accept all truthy values (`true`, `1`, `yes`, `on`,
    // case-insensitive) instead of only `"True"` and `"1"`.
    let toggle = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_TOGGLE")
        .map(|(_, v)| parse_toggle_value(v))
        .unwrap_or(false);
    let mode = if toggle {
        crate::hotkey::coordinator::Mode::Toggle
    } else {
        crate::hotkey::coordinator::Mode::HoldToTalk
    };
    if rust_session_sink::dictate_backend_rust_session_requested() {
        install_session_sink_hotkey(key_names, mode, tx, repaint_notifier)
    } else {
        // Logger sink does not need the notifier -- the existing
        // stream_lines path already invokes the repaint_notifier after
        // every event it enqueues, and the logger sink's outputs flow
        // through the same channel without the in-process bypass that
        // the session sink uses.
        let _ = repaint_notifier;
        install_logger_sink_hotkey(key_names, mode, tx)
    }
}

/// The historical (PR 1-3) logger sink: turn every
/// [`crate::hotkey::coordinator::CoordinatorAction`] into a stderr line
/// on the runtime event channel. Production still uses this -- the
/// Python worker owns the recording lifecycle until PR 6.
fn install_logger_sink_hotkey(
    key_names: Vec<String>,
    mode: crate::hotkey::coordinator::Mode,
    tx: std::sync::mpsc::Sender<RuntimeEvent>,
) -> Option<crate::hotkey::HotkeyHandle> {
    maybe_install_rust_hotkey(key_names, mode, move |action| {
        use crate::hotkey::coordinator::CoordinatorAction;
        let msg = match action {
            CoordinatorAction::StartRecording(id) => {
                format!("[hotkey] start_recording id={id}")
            }
            CoordinatorAction::StopAndTranscribe(id) => {
                format!("[hotkey] stop_and_transcribe id={id}")
            }
            CoordinatorAction::CancelRecording(id) => {
                format!("[hotkey] cancel_recording id={id}")
            }
        };
        let _ = tx.send(RuntimeEvent::Stderr(msg));
    })
}

/// Wave 5 PR 4 of #348: opt-in session-backed sink.
///
/// Wires the coordinator into a [`crate::dictate::DictateSession`] so
/// PTT press/release actually drives `session.start()` /
/// `stop_and_transcribe()` / `cancel(epoch)`. The session feeds
/// worker events back through `tx` as
/// [`RuntimeEvent::Worker`] just like the Python worker does, so the
/// UI's log card / tray-state machine see the same shape.
///
/// After `install_hotkey` succeeds, the
/// [`crate::hotkey::HotkeyHandle::coordinator_handle`] is poured into
/// the shared `OnceLock` the sink captured, so the sink's
/// `processing_finished` callback can send
/// [`crate::hotkey::coordinator::CoordinatorEvent::ProcessingFinished`]
/// back into the coordinator (the press latch otherwise stays parked
/// in `Stage::Processing`). On stub builds (no `rust-hotkeys`
/// feature) `maybe_install_rust_hotkey` returns `None` before we ever
/// touch the slot, so the cfg-gated accessor is only invoked on
/// builds where it exists.
fn install_session_sink_hotkey(
    key_names: Vec<String>,
    mode: crate::hotkey::coordinator::Mode,
    tx: std::sync::mpsc::Sender<RuntimeEvent>,
    repaint_notifier: Option<RepaintNotifier>,
) -> Option<crate::hotkey::HotkeyHandle> {
    let (sink, coord_slot) = rust_session_sink::build_production_sink(tx.clone(), repaint_notifier);
    let handle = maybe_install_rust_hotkey(key_names, mode, sink)?;
    #[cfg(feature = "rust-hotkeys")]
    {
        // `set` returns Err iff the slot is already populated. This is
        // the only writer; a duplicate `Some(handle)` from a future
        // refactor would be a programming error worth logging but not
        // panicking on -- the existing handle would still drive
        // ProcessingFinished correctly.
        if coord_slot.set(handle.coordinator_handle()).is_err() {
            let _ = tx.send(RuntimeEvent::Stderr(
                "[rust-session] coordinator handle slot already populated; \
                 ignoring (this is a no-op but indicates a refactor regression)"
                    .to_owned(),
            ));
        }
    }
    // Suppress `unused` warnings when `rust-hotkeys` is off: the slot
    // would never be populated, but `build_production_sink` already
    // returned it; explicitly discard so clippy stays quiet.
    #[cfg(not(feature = "rust-hotkeys"))]
    let _ = coord_slot;
    Some(handle)
}
