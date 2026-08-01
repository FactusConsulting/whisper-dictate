//! `whisper-dictate self-test hotkey-boot` — Windows PTT-boot
//! regression test.
//!
//! ## What this catches
//!
//! The Windows PTT bug that motivated this verb: launching
//! `whisper-dictate-gui.exe` with `VOICEPI_DICTATE_ENGINE=rust`
//! resulted in the chord firing NO PTT event on Windows, while the
//! same configuration worked on Ubuntu and the CLI `dictate-run`
//! path worked on Windows. The GUI's `windows_subsystem = "windows"`
//! attribute discards stderr, so any rdev listener-startup failure
//! was invisible.
//!
//! This verb exercises the SAME [`crate::hotkey::install_hotkey`]
//! path the Phase-B supervisor uses (via
//! [`crate::runtime::in_process::try_install`]) but drives it from
//! the console-attached CLI so failures surface on stderr AND in the
//! JSON envelope.
//!
//! ## Scope
//!
//! * **Installs** — feature detection, chord validation, rdev/evdev
//!   spawn, coordinator wiring. If any step fails, this verb
//!   surfaces the underlying [`crate::hotkey::InstallError`] and
//!   exits non-zero.
//! * **Verifies stay-alive** — after install, holds the handle for
//!   `--for` seconds and confirms the listener did not exit early.
//!   The rdev Windows listener does a single blocking `GetMessageA`
//!   call; if it ever returns, the LL_KEYBOARD hook installed
//!   against the listener thread dies with the thread. This verb's
//!   `listener_exited_early` flag catches that class of regression.
//! * **Does NOT install audio pump / Whisper model** — the goal is
//!   the OS-hook and coordinator wiring only, so this stays fast and
//!   hermetic. For the full boot chain use `dictate-run` (which does
//!   open the audio pump + load the model).
//!
//! ## JSON contract
//!
//! Emitted with `--json`. Keys are stable; a rename is a breaking
//! change that requires an updated smoke script.
//!
//! ```json
//! {
//!   "kind": "hotkey_boot_self_test",
//!   "ok": true|false,
//!   "driver": "rdev"|"evdev"|"none",
//!   "chord": "ctrl_l+shift_l",
//!   "install_ms": 42,
//!   "listener_exited_early": false,
//!   "install_error": null | "…error string…"
//! }
//! ```

use serde_json::json;

/// One boot-test outcome — populated whether install succeeded or
/// not, so the JSON envelope is always the same shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootReport {
    /// Driver name reported by the successful install
    /// ([`crate::hotkey::HotkeyHandle::driver_name`]), or `"none"` on
    /// install failure.
    pub driver: &'static str,
    /// The chord that was resolved (either the `--chord` override or
    /// the config's `key` setting, joined on `+`). Present even on
    /// install failure so the operator sees exactly what the verb
    /// tried to install.
    pub chord: String,
    /// Wall-clock milliseconds from install start to install return.
    /// On error this is the time until the error was surfaced.
    pub install_ms: u128,
    /// True if the listener thread exited before the `--for` window
    /// completed. Only meaningful when `install_error` is `None`.
    /// Windows regression this catches: rdev's `listen` returning
    /// prematurely and the LL hook dying with the thread.
    ///
    /// Reads
    /// [`crate::hotkey::HotkeyHandle::is_listener_alive`], which the
    /// rdev driver keeps up to date via a shared atomic the listener
    /// thread flips to `false` on exit (normal return, Err, or panic).
    /// This is a DIRECT liveness signal — a `true` reading is an
    /// unambiguous "the OS hook is dead"; unlike the earlier
    /// `driver_name()`-stability heuristic, it cannot report `false`
    /// for a listener that ended mid-window. #644
    /// r3658983542.
    pub listener_exited_early: bool,
    /// Error string from [`crate::hotkey::install_hotkey`] when
    /// install failed. `None` on success.
    pub install_error: Option<String>,
}

impl BootReport {
    /// True when install succeeded AND the listener stayed alive for
    /// the full window. The verb's exit code follows this.
    pub fn ok(&self) -> bool {
        self.install_error.is_none() && !self.listener_exited_early
    }

    /// Render as one JSON object per the module docs. Callers wrap
    /// this in `println!` — the newline discipline stays with the
    /// caller so a redirect to a file gets exactly one line.
    pub fn to_json(&self) -> String {
        json!({
            "kind": "hotkey_boot_self_test",
            "ok": self.ok(),
            "driver": self.driver,
            "chord": self.chord,
            "install_ms": self.install_ms,
            "listener_exited_early": self.listener_exited_early,
            "install_error": self.install_error,
        })
        .to_string()
    }

    /// Human-readable summary — matches the shape of the other
    /// self-test verbs' plain output (`[self-test <verb>] key=value
    /// ...`). Grep target: `[self-test hotkey-boot]`.
    pub fn to_plain(&self) -> String {
        let mut out = format!(
            "[self-test hotkey-boot] driver={} chord={} install_ms={} listener_exited_early={}",
            self.driver, self.chord, self.install_ms, self.listener_exited_early,
        );
        if let Some(err) = &self.install_error {
            out.push_str(&format!(" install_error={err:?}"));
        }
        if self.ok() {
            out.push_str(" -> PASS");
        } else {
            out.push_str(" -> FAIL");
        }
        out
    }
}

/// Whether this build has the features `hotkey-boot` needs. Consulted
/// by the CLI dispatcher to print an actionable rebuild message
/// instead of running an empty stub.
pub const fn features_available() -> bool {
    cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
}

/// Feature-gated implementation. On a stock build the CLI wrapper
/// prints the rebuild message before reaching here; the stub keeps
/// the same signature so the dispatcher does not need `#[cfg]` at
/// every call site.
#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub fn run_boot_test(chord: String, hold_ms: u64) -> BootReport {
    use crate::hotkey::{install_hotkey, HotkeyConfig};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let cfg = HotkeyConfig {
        key_names: chord
            .split('+')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        mode: crate::hotkey::coordinator::Mode::HoldToTalk,
        // We are NOT running the session sink here — the coordinator
        // exists only to accept the tracker's outputs during the
        // stay-alive window. `auto_complete_processing` lets a
        // hypothetical press+release cycle round-trip cleanly.
        auto_complete_processing: true,
    };

    // Discard sink — we do not exercise the recording lifecycle;
    // installing the hook + coordinator wiring is the ENTIRE test.
    let (tx, _rx) = mpsc::channel();
    let install_result = install_hotkey(cfg, move |action| {
        let _ = tx.send(action);
    });
    let install_ms = start.elapsed().as_millis();

    match install_result {
        Ok(handle) => {
            let driver = handle.driver_name();
            // Hold the handle for the requested window. The rdev /
            // evdev listener threads keep running in the background;
            // if either exits, the driver_name stays the same
            // (`&'static str`) but the OS hook is dead. #644
            // r3658983542: `HotkeyHandle::is_listener_alive` now
            // provides a direct signal — the rdev driver flips a
            // shared atomic to `false` when its listener thread ends
            // for ANY reason (Err quick-failure raced past the ready
            // gate, an unexpected Ok return, or a panic). Poll it
            // after the hold window so a listener that exited during
            // the window is reported as `listener_exited_early: true`
            // and the self-test's own smoke script catches the class
            // of regression this verb was written for.
            std::thread::sleep(Duration::from_millis(hold_ms));
            let listener_exited_early = !handle.is_listener_alive();
            // Explicit shutdown so the manager thread joins before
            // we return. The rdev listener thread is unjoinable —
            // the OS listener stays running until process exit
            // (documented rdev limitation), which is fine because
            // this is a one-shot CLI verb.
            handle.shutdown();
            BootReport {
                driver,
                chord,
                install_ms,
                listener_exited_early,
                install_error: None,
            }
        }
        Err(err) => BootReport {
            driver: "none",
            chord,
            install_ms,
            listener_exited_early: false,
            install_error: Some(err.to_string()),
        },
    }
}

/// Stock-build stub — never reached at runtime because the CLI
/// dispatcher prints the rebuild message via [`features_available`].
/// Kept for symmetry with the sibling `self_test.rs` module.
#[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
pub fn run_boot_test(chord: String, _hold_ms: u64) -> BootReport {
    BootReport {
        driver: "none",
        chord,
        install_ms: 0,
        listener_exited_early: false,
        install_error: Some(
            "hotkey-boot requires the `rust-hotkeys` and `rust-injection` cargo features"
                .to_owned(),
        ),
    }
}

/// Reconcile a `--chord` override with a config-load result. Extracted
/// from `handle_self_test_hotkey_boot` so the "propagate the load error
/// when there is no override, otherwise warn-and-continue" branching
/// (#644  r3658983556) is directly unit-testable.
///
/// * `override_value` — the raw `--chord` CLI argument (may be empty).
/// * `config_load` — the `Result` returned by
///   [`crate::config::load_settings`], projected to just the `key`
///   field on `Ok` and the stringified error on `Err`.
///
/// Returns `Ok(config_key_or_empty)` when it is safe to fall through
/// to [`resolve_chord`], and `Err(reason)` when the operator should
/// see the original config-load failure verbatim. The pre-fix code
/// used `unwrap_or_default()` here, which discarded any Err and turned
/// it into the misleading "no PTT chord configured" message
/// downstream — the exact regression this helper's Err branch pins.
pub fn reconcile_config_load(
    override_value: &str,
    config_load: std::result::Result<String, String>,
) -> std::result::Result<String, String> {
    match config_load {
        Ok(key) => Ok(key),
        Err(err) if !override_value.trim().is_empty() => {
            // Override supplied: the config's `key` value isn't going
            // to be consulted anyway, so a load failure isn't fatal.
            // The caller can still emit a warning line before proceeding.
            let _ = err;
            Ok(String::new())
        }
        Err(err) => Err(format!(
            "failed to load current config for hotkey-boot self-test: {err}; \
             supply `--chord <chord>` to bypass the config lookup"
        )),
    }
}

/// Resolve the chord to install: honour an explicit `--chord`
/// override, otherwise read the current config's `key` field the
/// exact same way the supervisor does. Split out so the pure
/// resolution can be unit-tested without touching disk.
pub fn resolve_chord(override_value: &str, config_key: &str) -> String {
    let raw = if override_value.trim().is_empty() {
        config_key
    } else {
        override_value
    };
    // Normalise whitespace so `"ctrl_l + shift_l"` and
    // `"ctrl_l+shift_l"` install the same chord.
    raw.split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("+")
}

// Unit tests moved to the companion `boot_self_test_tests.rs` file so the
// regression-test discipline scanner (per AGENTS.md, see
// `src/tests/python/test_regression_test_discipline.py`) sees a matching
// test file next to the production module. Sonar quality-gate feedback
// on PR #668 required the split: an inline `#[cfg(test)] mod tests` in
// this file did not satisfy the scanner's `foo.rs` -> `foo_tests.rs`
// lookup when `reconcile_config_load` was introduced.
