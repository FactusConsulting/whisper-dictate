//! `wd dictate-run` — foreground CLI verb that installs the
//! full Rust dictation runtime (hotkey listener + coordinator + session sink +
//! real backends when the required features are compiled in) and runs until
//! Ctrl-C.
//!
//! The hidden `dictate-run` verb originally shipped as the Phase A bridge from
//! Python. It is now also the implementation behind the public
//! `wd run` Rust route, so terminal startup does not need to
//! resolve or launch Python first.
//!
//! ## What the verb does at run time
//!
//! 1. Loads config (`--config PATH`, else the platform user config honouring
//!    `VOICEPI_CONFIG`).
//! 2. Builds the same production session action-sink the supervisor uses at
//!    `runtime::rust_session_sink::build_production_sink`. With
//!    `--features whisper-rs-local,rust-injection,audio-capture` all present
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

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
use super::dictate_run_output::{emit_event, emit_ready, emit_shutdown};

/// Parsed `dictate-run` arguments, in the shape the handler consumes.
/// Kept as a plain struct (not a clap-derived one) so the CLI enum stays
/// self-describing and the handler is easy to invoke from tests or a future
/// programmatic entry point without going through clap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DictateRunArgs {
    pub config: Option<String>,
    pub json_events: bool,
    pub foreground: bool,
    /// Per-invocation CLI overrides applied after config materialisation.
    /// This preserves the historical `run --key/--lang/...` precedence
    /// without mutating the user's saved config.
    pub env_overrides: Vec<(String, String)>,
}

/// CLI entry point. Split from the internals so the stock-build stub keeps
/// the same signature.
pub fn handle_dictate_run(args: DictateRunArgs) -> Result<()> {
    if !features_available() {
        return Err(anyhow!(
            "dictate-run requires the `rust-hotkeys` and `rust-injection` cargo features - \
             rebuild with `cargo build --features rust-hotkeys,rust-injection` (Phase A step 1 \
             of the native runtime; see docs/ARCHITECTURE.md)"
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

/// Whether this build can serve a complete native terminal session instead of
/// merely installing the hotkey/sink wiring. Reduced Linux source builds omit
/// these heavier features and retain the Python compatibility path.
pub const fn production_features_available() -> bool {
    cfg!(all(
        feature = "rust-hotkeys",
        feature = "rust-injection",
        feature = "audio-capture",
        feature = "whisper-rs-local"
    ))
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
         - this is a bug in the CLI dispatcher"
    ))
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
fn run(args: DictateRunArgs) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::Arc;
    use std::time::Duration;

    use crate::hotkey::{coordinator, install_hotkey, HotkeyConfig, InstallError};
    use crate::runtime::rust_session_sink;

    let DictateRunArgs {
        config,
        json_events: cli_json_events,
        foreground,
        env_overrides,
    } = args;
    // `--foreground` is currently a documentation flag: this verb never
    // daemonises (the whole process IS the dictation runtime), so the flag
    // is a no-op today. Kept in the CLI so the Phase A step 2 Python
    // dispatch can pass it through explicitly, and so the design stays
    // symmetric with the eventual supervisor-mode branch (Phase B, where a
    // background variant may exist). The `_ = foreground` binding pins the
    // parameter as intentional so `-D warnings` stays quiet.
    let _ = foreground;

    // 1. Resolve config, ambient values and CLI overrides into one owned
    // snapshot. An explicit path is read directly; it is never installed as
    // process-global VOICEPI_CONFIG.
    let raw_config = match config.as_deref() {
        Some(p) => crate::config::load_raw_config_from_path(Path::new(p))?,
        None => crate::config::load_raw_config()?,
    };
    let ambient_live_env = crate::config::ambient_live_runtime_env();
    let runtime_env = crate::config::effective_runtime_env_from_raw(&raw_config);
    let mut runtime = super::settings_snapshot::RuntimeSettingsSnapshot::from_pairs_with_ambient(
        runtime_env,
        |name| std::env::var(name).ok(),
    )?;
    if let Ok(settings) = crate::config::AppSettings::from_value(raw_config.clone()) {
        runtime.set_stt_provider(settings.stt_provider);
    }
    let forced_live_env: std::collections::BTreeMap<String, String> =
        env_overrides.iter().cloned().collect();
    for (name, value) in env_overrides {
        runtime.set(name, value)?;
    }
    crate::runtime::cloud_api_keys::attach_cloud_api_keys(&mut runtime)?;
    let settings = runtime.settings();
    let key_names = split_key_names(&settings.key);
    let toggle_mode = settings.toggle_mode;
    if key_names.is_empty() {
        return Err(anyhow!(
            "no PTT chord configured (settings.key is empty in the resolved config); \
             set one via `wd config set key ctrl_l+shift_l` and retry"
        ));
    }
    let display_chord = key_names.join("+");
    let mode = if toggle_mode {
        coordinator::Mode::Toggle
    } else {
        coordinator::Mode::HoldToTalk
    };
    let cfg = HotkeyConfig {
        key_names: key_names.clone(),
        mode,
        // Shipping runtime: the session sink drives `ProcessingFinished`
        // from the real transcription pass, so no synthetic completion.
        auto_complete_processing: false,
    };

    let json_events = effective_json_events(cli_json_events, runtime.value("VOICEPI_JSON"));
    validate_native_runtime_options(
        Some(&settings.device),
        Some(&settings.stt_backend),
        cfg!(feature = "whisper-rs-vulkan"),
    )?;

    // 2. Build the production session action-sink. Mirrors the supervisor's
    //    setup path (`runtime::supervisor::RuntimeSupervisor::start` when
    //    the rust-session backend is requested) — same helper, so a change
    //    to one is felt by the other.
    let (tx, rx) = mpsc::channel();
    let live_env_overrides = super::live_settings::LiveEnvOverrides {
        ambient: ambient_live_env,
        forced: forced_live_env,
        config_path: config.map(std::path::PathBuf::from),
        post_processor: runtime
            .value("VOICEPI_POST_PROCESSOR")
            .unwrap_or_default()
            .to_owned(),
    };
    let (sink, coord_slot, runtime_active, capture_stop) =
        rust_session_sink::try_build_production_sink(tx.clone(), None, live_env_overrides, runtime)
            .map_err(|err| anyhow!("native dictation backend could not start: {err}"))?;

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
                 being on - this is a build-configuration bug"
            ));
        }
        Err(err @ InstallError::EmptyConfig) => return Err(err.into()),
        Err(err @ InstallError::UnsupportedKey(_)) => return Err(err.into()),
        // The 2026-07-29 pairing, seen from the CLI side: a tray GUI was
        // already holding F9 when this verb started. Refusing here is what
        // stops both processes from typing into the focused window at
        // once; the error text names the pid to quit and the corruption it
        // prevented (`hotkey::ptt_lock`). Returned verbatim so the console
        // operator reads the whole account on stderr.
        Err(err @ InstallError::AlreadyHeld { .. }) => return Err(err.into()),
        Err(InstallError::ListenerStartup(msg)) => {
            return Err(anyhow!(
                "hotkey listener failed to start ({msg}); on Linux without an X display \
                 this is expected - retry from a real user session, or use the evdev \
                 backend if you have `/dev/input/*` permissions"
            ));
        }
    };
    // Wire the coordinator handle so the sink's `on_processing_finished`
    // can fire ProcessingFinished after every stop completes (unblocks the
    // Stage::Processing guard so the next PTT press is acted on).
    let _ = coord_slot.set(handle.coordinator_handle());
    // The sink/audio/hotkey components now own every live sender. Keeping the
    // construction root here would make `Disconnected` unreachable after all
    // components stop, leaving this foreground loop polling forever.
    release_root_sender(tx);

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
    let mut listener_failure = false;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            emit_shutdown(json_events, "ctrl-c");
            break;
        }
        if !handle.is_listener_alive() {
            crate::diag::log!(
                "[dictate-run] hotkey listener exited; stopping because push-to-talk is unavailable"
            );
            emit_shutdown(json_events, "hotkey-listener-exited");
            listener_failure = true;
            break;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => {
                emit_event(json_events, &event);
                if terminal_event_ends_runtime(&event) {
                    crate::diag::log!(
                        "[dictate-run] terminal runtime event received; closing injection gate"
                    );
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                emit_shutdown(json_events, "channel-disconnected");
                break;
            }
        }
    }

    // 7. Close injection before waiting for any synchronous transcription
    // action to finish. Ctrl-C and terminal capture failure must never allow
    // a late result to type after the operator requested shutdown.
    runtime_active.store(false, Ordering::Release);
    capture_stop();
    if crate::diag::debug_enabled() {
        crate::diag::log!("[dictate-run/debug] injection lifecycle gate active=false");
    }

    // 8. Explicit shutdown so the manager + coordinator threads join before
    //    we drop back into `main`. Drop would also do it, but making the
    //    order explicit avoids the last-second thread teardown running after
    //    stdout has been closed by the runtime.
    handle.shutdown();
    if listener_failure {
        Err(anyhow!(
            "native hotkey listener exited; inspect debug/trace diagnostics for the backend failure"
        ))
    } else {
        Ok(())
    }
}

#[cfg_attr(
    not(all(feature = "rust-hotkeys", feature = "rust-injection")),
    allow(dead_code)
)]
fn terminal_event_ends_runtime(event: &crate::runtime::RuntimeEvent) -> bool {
    matches!(event, crate::runtime::RuntimeEvent::Exited { .. })
}

#[cfg_attr(
    not(all(feature = "rust-hotkeys", feature = "rust-injection")),
    allow(dead_code)
)]
fn effective_json_events(cli_enabled: bool, env_value: Option<&str>) -> bool {
    cli_enabled || env_value.is_some_and(parse_truthy)
}

fn parse_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

#[cfg_attr(
    not(all(feature = "rust-hotkeys", feature = "rust-injection")),
    allow(dead_code)
)]
fn release_root_sender<T>(sender: std::sync::mpsc::Sender<T>) {
    drop(sender);
}

#[cfg_attr(
    not(all(feature = "rust-hotkeys", feature = "rust-injection")),
    allow(dead_code)
)]
pub(super) fn validate_native_runtime_options(
    device: Option<&str>,
    stt_backend: Option<&str>,
    gpu_backend_compiled: bool,
) -> Result<()> {
    let cloud_transcription =
        stt_backend.is_some_and(|value| value.trim().eq_ignore_ascii_case("openai"));
    let requests_gpu = device.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "vulkan" | "cuda"
        )
    });
    if !cloud_transcription && !gpu_backend_compiled && requests_gpu {
        return Err(anyhow!(
            "the native Rust runtime cannot honor device=vulkan in this CPU-only build; \
             use cpu/auto or install a GPU-enabled release"
        ));
    }
    Ok(())
}

/// Split the PTT `settings.key` string into individual key names. Mirrors
/// [`crate::hotkey::capture::split_key_names`] byte-for-byte — copied here
/// (rather than re-exported) so this module stays a leaf that compiles even
/// when `capture` grows a future dep-chain we don't need. Same trimming +
/// empty-segment rules as the shipping runtime's
/// the in-process supervisor's chord parser, so a config that installs
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

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[path = "dictate_run_tests.rs"]
mod tests;
