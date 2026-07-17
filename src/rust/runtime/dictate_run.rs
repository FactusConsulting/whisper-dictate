//! `whisper-dictate dictate-run` — foreground CLI verb that installs the
//! full Rust dictation runtime (hotkey listener + coordinator + session sink +
//! real backends when the required features are compiled in) and runs until
//! Ctrl-C.
//!
//! Audit item 5 Phase A step 1 (see
//! [`docs/design/item5-wire-dictate-session.md`]). **This verb ships the
//! plumbing only — the Python entrypoint at
//! `src/python/whisper_dictate/runtime.py` still runs the shipping PTT loop
//! unconditionally.** A follow-up PR (Phase A step 2) will branch on
//! `VOICEPI_DICTATE_ENGINE` at `_run_session` and shell out to this verb when
//! the operator has opted in. Nothing here changes production behaviour on
//! its own.
//!
//! ## What the verb does at run time
//!
//! 1. Loads config (`--config PATH`, else the platform user config honouring
//!    `VOICEPI_CONFIG`).
//! 2. Builds the same production session action-sink the supervisor uses at
//!    `runtime::rust_session_sink::build_production_sink`. With
//!    `--features whisper-rs-local,rust-injection,audio-in-rust` all present
//!    the sink drives the real `WhisperLocalTranscribeBackend` /
//!    `EnigoInjectBackend` + audio pump; otherwise it falls back to the PR 4
//!    stub session so the wire-up still installs (matches the supervisor's
//!    behaviour byte-for-byte).
//! 3. Installs the Rust hotkey subsystem via
//!    [`crate::hotkey::install_hotkey`] with the sink as the action target,
//!    populates the coordinator-handle slot so `ProcessingFinished` can loop
//!    back after a stop completes, then runs the event loop until either a
//!    Ctrl-C fires or the runtime channel disconnects.
//! 4. On `--json-events`, emits a `{"ready":true,"engine":"rust"}` line
//!    BEFORE the loop starts (so a supervising Python parent can gate on it)
//!    and then one JSON object per line for every `RuntimeEvent` seen. On
//!    plain output the same information is rendered as human-readable
//!    `[dictate-run] …` lines.
//!
//! ## Feature gating
//!
//! Requires both `rust-hotkeys` (for the coordinator + hotkey listener) and
//! `rust-injection` (for the shared self-injection guard the coordinator
//! reads on Windows). A stock build exposes the CLI verb so the surface stays
//! stable across feature configurations but exits non-zero with an
//! actionable "rebuild with --features …" message — matching the policy the
//! `self-test ptt-wedge` verb established in `main.rs::handle_self_test`.

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
use std::path::Path;

use anyhow::{anyhow, Result};

/// Parsed `dictate-run` arguments, in the shape the handler consumes.
/// Kept as a plain struct (not a clap-derived one) so the CLI enum stays
/// self-describing and the handler is easy to invoke from tests or a future
/// programmatic entry point without going through clap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DictateRunArgs {
    pub config: Option<String>,
    pub json_events: bool,
    pub foreground: bool,
}

/// CLI entry point. Split from the internals so the stock-build stub keeps
/// the same signature.
pub fn handle_dictate_run(args: DictateRunArgs) -> Result<()> {
    if !features_available() {
        return Err(anyhow!(
            "dictate-run requires the `rust-hotkeys` and `rust-injection` cargo features — \
             rebuild with `cargo build --features rust-hotkeys,rust-injection` (Phase A step 1 \
             of audit item 5; see docs/design/item5-wire-dictate-session.md)"
        ));
    }
    run(args)
}

/// Whether this build carries the features the verb needs to actually
/// install a listener + sink. Consulted at handler entry so a stock build
/// gets an actionable rebuild message rather than a mysterious "hotkey
/// unsupported" install error.
pub const fn features_available() -> bool {
    cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
}

// Stub path: keeps the compiler quiet on `_args` when the features are off
// AND lets `handle_dictate_run` above call `run(args)` unconditionally with
// the same signature.
#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
fn run(_args: DictateRunArgs) -> Result<()> {
    // Unreachable: `handle_dictate_run` returned early via `features_available()`.
    // Kept here (rather than `unreachable!()`) so a future refactor that moves
    // the gate can't turn this into a silent no-op.
    Err(anyhow!(
        "dictate-run stub reached on a build without rust-hotkeys+rust-injection \
         — this is a bug in the CLI dispatcher"
    ))
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
fn run(args: DictateRunArgs) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::Arc;
    use std::time::Duration;

    use crate::config::{load_settings, load_settings_from_path};
    use crate::hotkey::{coordinator, install_hotkey, HotkeyConfig, InstallError};
    use crate::runtime::rust_session_sink;

    let DictateRunArgs {
        config,
        json_events,
        foreground,
    } = args;
    // `--foreground` is currently a documentation flag: this verb never
    // daemonises (the whole process IS the dictation runtime), so the flag
    // is a no-op today. Kept in the CLI so the Phase A step 2 Python
    // dispatch can pass it through explicitly, and so the design stays
    // symmetric with the eventual supervisor-mode branch (Phase B, where a
    // background variant may exist). The `_ = foreground` binding pins the
    // parameter as intentional so `-D warnings` stays quiet.
    let _ = foreground;

    // 1. Load settings + PTT chord.
    let settings = match config.as_deref() {
        Some(p) => load_settings_from_path(Path::new(p))?,
        None => load_settings()?,
    };
    let key_names = split_key_names(&settings.key);
    if key_names.is_empty() {
        return Err(anyhow!(
            "no PTT chord configured (settings.key is empty in the resolved config); \
             set one via `whisper-dictate config set key ctrl_l+shift_l` and retry"
        ));
    }
    let display_chord = key_names.join("+");
    let mode = if settings.toggle_mode {
        coordinator::Mode::Toggle
    } else {
        coordinator::Mode::HoldToTalk
    };
    let cfg = HotkeyConfig {
        key_names: key_names.clone(),
        mode,
    };

    // 2. Build the production session action-sink. Mirrors the supervisor's
    //    setup path (`runtime::supervisor::RuntimeSupervisor::start` when
    //    the rust-session backend is requested) — same helper, so a change
    //    to one is felt by the other.
    let (tx, rx) = mpsc::channel();
    let (sink, coord_slot) = rust_session_sink::build_production_sink(tx.clone(), None);

    // 3. Install the hotkey subsystem with the sink as the action target.
    // Pass the boxed sink directly; clippy's `redundant_closure` lint won't
    // accept a wrapping closure here and `Box<dyn FnMut(...)+Send+'static>`
    // itself satisfies the `FnMut(...)+Send+'static` bound install_hotkey
    // requires (via auto-deref on the Box).
    let install_res = install_hotkey(cfg, sink);
    let handle = match install_res {
        Ok(h) => h,
        Err(InstallError::Unsupported) => {
            return Err(anyhow!(
                "hotkey install returned Unsupported despite the `rust-hotkeys` feature \
                 being on — this is a build-configuration bug"
            ));
        }
        Err(err @ InstallError::EmptyConfig) => return Err(err.into()),
        Err(err @ InstallError::UnsupportedKey(_)) => return Err(err.into()),
        Err(InstallError::ListenerStartup(msg)) => {
            return Err(anyhow!(
                "hotkey listener failed to start ({msg}); on Linux without an X display \
                 this is expected — retry from a real user session, or use the evdev \
                 backend if you have `/dev/input/*` permissions"
            ));
        }
    };
    // Wire the coordinator handle so the sink's `on_processing_finished`
    // can fire ProcessingFinished after every stop completes (unblocks the
    // Stage::Processing guard so the next PTT press is acted on).
    let _ = coord_slot.set(handle.coordinator_handle());

    // 4. Install the Ctrl-C handler. `ctrlc::set_handler` is process-wide
    //    and one-shot: a second install returns an error. In practice
    //    dictate-run runs at most once per process (it does not return
    //    unless the operator quit or a fatal error tripped), so best-effort
    //    is enough — if a prior verb already installed a handler we
    //    inherit theirs.
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_signal = Arc::clone(&shutdown);
    if let Err(err) = ctrlc::set_handler(move || {
        shutdown_signal.store(true, Ordering::SeqCst);
    }) {
        eprintln!(
            "[dictate-run] warning: could not install Ctrl-C handler ({err}); \
             the runtime will still exit on any RuntimeEvent::Exited signal"
        );
    }

    // 5. Emit the ready signal. Placed AFTER install so a Python parent
    //    gating on `{"ready":true}` knows the hotkey listener is live.
    emit_ready(json_events, &display_chord, handle.driver_name());

    // 6. Drain the runtime event channel until Ctrl-C or disconnect.
    loop {
        if shutdown.load(Ordering::SeqCst) {
            emit_shutdown(json_events, "ctrl-c");
            break;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => emit_event(json_events, &event),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                emit_shutdown(json_events, "channel-disconnected");
                break;
            }
        }
    }

    // 7. Explicit shutdown so the manager + coordinator threads join before
    //    we drop back into `main`. Drop would also do it, but making the
    //    order explicit avoids the last-second thread teardown running after
    //    stdout has been closed by the runtime.
    handle.shutdown();
    Ok(())
}

/// Split the PTT `settings.key` string into individual key names. Mirrors
/// [`crate::hotkey::capture::split_key_names`] byte-for-byte — copied here
/// (rather than re-exported) so this module stays a leaf that compiles even
/// when `capture` grows a future dep-chain we don't need. Same trimming +
/// empty-segment rules as the shipping runtime's
/// `hotkey_install::extract_hotkey_key_names`, so a config that installs
/// under the Python worker installs identically here.
///
/// Always compiled (not feature-gated) so the tests below run on every
/// build and pin the config-parsing behaviour independently of whether the
/// runtime is wired.
#[cfg_attr(
    not(all(feature = "rust-hotkeys", feature = "rust-injection")),
    allow(dead_code)
)]
fn split_key_names(chord: &str) -> Vec<String> {
    chord
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

// ── output formatting ────────────────────────────────────────────────────────
//
// Pure functions so the routing is unit-testable without a live coordinator.
// The plain-text form is stable-ish (human logs) but the JSON keys are the
// machine-readable contract callers (Python parent, CI scripts) should pin.

/// Human-readable / JSON line prefix. Grep target for smoke scripts.
#[cfg_attr(
    not(all(feature = "rust-hotkeys", feature = "rust-injection")),
    allow(dead_code)
)]
pub(crate) const OUTPUT_PREFIX: &str = "[dictate-run]";

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
fn emit_ready(json: bool, chord: &str, driver: &'static str) {
    if json {
        let line = serde_json::json!({
            "kind": "ready",
            "ready": true,
            "engine": "rust",
            "chord": chord,
            "driver": driver,
        })
        .to_string();
        println!("{line}");
    } else {
        println!("{OUTPUT_PREFIX} ready (engine=rust, driver={driver}, chord={chord})");
    }
    // Best-effort flush so a Python parent gating on the line sees it
    // immediately rather than waiting for the OS to flush the stdio buffer
    // at the next newline.
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
fn emit_shutdown(json: bool, reason: &str) {
    if json {
        let line = serde_json::json!({
            "kind": "shutdown",
            "reason": reason,
        })
        .to_string();
        println!("{line}");
    } else {
        println!("{OUTPUT_PREFIX} shutdown (reason={reason})");
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
fn emit_event(json: bool, event: &crate::runtime::RuntimeEvent) {
    use crate::runtime::RuntimeEvent;
    if json {
        // Deliberately conservative shape: pass the WorkerEvent's own JSON
        // payload straight through so Python consumers can key off the same
        // event names they see today; wrap non-worker variants with a `kind`
        // tag so the stream stays parseable.
        let value = match event {
            RuntimeEvent::Worker(w) => serde_json::json!({
                "kind": "worker",
                "event": w.event,
                "state": w.state,
                "payload": w.payload,
            }),
            RuntimeEvent::Started { command } => serde_json::json!({
                "kind": "started",
                "command": command,
            }),
            RuntimeEvent::Stdout(line) => serde_json::json!({
                "kind": "stdout",
                "line": line,
            }),
            RuntimeEvent::Stderr(line) => serde_json::json!({
                "kind": "stderr",
                "line": line,
            }),
            RuntimeEvent::Exited { code } => serde_json::json!({
                "kind": "exited",
                "code": code,
            }),
            RuntimeEvent::Error(msg) => serde_json::json!({
                "kind": "error",
                "message": msg,
            }),
        };
        println!("{value}");
    } else {
        match event {
            RuntimeEvent::Worker(w) => println!(
                "{OUTPUT_PREFIX} worker event={} state={:?}",
                w.event, w.state
            ),
            RuntimeEvent::Started { command } => {
                println!("{OUTPUT_PREFIX} started ({command})")
            }
            RuntimeEvent::Stdout(line) => println!("{OUTPUT_PREFIX} stdout: {line}"),
            RuntimeEvent::Stderr(line) => println!("{OUTPUT_PREFIX} stderr: {line}"),
            RuntimeEvent::Exited { code } => {
                println!("{OUTPUT_PREFIX} exited (code={code:?})")
            }
            RuntimeEvent::Error(msg) => println!("{OUTPUT_PREFIX} error: {msg}"),
        }
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_key_names_single_key() {
        assert_eq!(split_key_names("ctrl_r"), vec!["ctrl_r".to_owned()]);
    }

    #[test]
    fn split_key_names_multi_key_chord() {
        assert_eq!(
            split_key_names("ctrl_l+shift_l+l"),
            vec!["ctrl_l".to_owned(), "shift_l".to_owned(), "l".to_owned()]
        );
    }

    #[test]
    fn split_key_names_trims_and_drops_empty() {
        // Mirrors `hotkey::capture::split_key_names` so a config that
        // installs under `hotkey capture` installs identically here.
        assert_eq!(
            split_key_names("  ctrl_l +  + shift_r "),
            vec!["ctrl_l".to_owned(), "shift_r".to_owned()]
        );
    }

    #[test]
    fn split_key_names_empty_input_yields_empty_vec() {
        assert!(split_key_names("").is_empty());
        assert!(split_key_names("   ").is_empty());
        assert!(split_key_names("+ + +").is_empty());
    }

    #[test]
    fn features_available_matches_cfg() {
        // Pin the gate so a refactor of `cfg!` at the call site is caught.
        assert_eq!(
            features_available(),
            cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
        );
    }

    #[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
    #[test]
    fn stock_build_returns_actionable_rebuild_message() {
        // The stock build MUST NOT install anything — it should fail fast
        // with a message that names the missing features and the rebuild
        // command. This is the contract the Python parent (Phase A step 2)
        // will rely on to distinguish "feature not built" from a runtime
        // failure it should surface.
        let err = handle_dictate_run(DictateRunArgs {
            config: None,
            json_events: false,
            foreground: false,
        })
        .expect_err("stock build must refuse dictate-run");
        let msg = err.to_string();
        assert!(
            msg.contains("rust-hotkeys") && msg.contains("rust-injection"),
            "error must name both required features: {msg}"
        );
        assert!(
            msg.contains("cargo build --features"),
            "error must include the rebuild command: {msg}"
        );
    }
}
