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
    /// for a listener that ended mid-window. Codex P1 #644 discussion
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
            // (`&'static str`) but the OS hook is dead. Codex P1 #644
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
/// (Codex P2 #644 discussion r3658983556) is directly unit-testable.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_chord_prefers_override_when_present() {
        assert_eq!(resolve_chord("ctrl_l", "shift_r"), "ctrl_l");
    }

    #[test]
    fn resolve_chord_falls_back_to_config_when_override_is_blank() {
        assert_eq!(resolve_chord("", "ctrl_l+shift_l"), "ctrl_l+shift_l");
        // Whitespace-only override still counts as "blank" so a user
        // that passes `--chord ""` on the shell doesn't accidentally
        // install a nameless chord.
        assert_eq!(resolve_chord("  ", "ctrl_l"), "ctrl_l");
    }

    #[test]
    fn resolve_chord_normalises_whitespace_around_plus_separators() {
        assert_eq!(
            resolve_chord("ctrl_l + shift_l", ""),
            "ctrl_l+shift_l",
            "override must be normalised so `--chord \"a + b\"` matches the on-disk `a+b`",
        );
    }

    #[test]
    fn resolve_chord_drops_empty_segments() {
        // Guards against a `key = "ctrl_l++f9"` config from confusing
        // the install-time validator downstream.
        assert_eq!(resolve_chord("", "ctrl_l++f9"), "ctrl_l+f9");
    }

    #[test]
    fn features_available_matches_cfg() {
        assert_eq!(
            features_available(),
            cfg!(all(feature = "rust-hotkeys", feature = "rust-injection"))
        );
    }

    #[test]
    fn report_ok_is_true_only_on_clean_install_and_healthy_listener() {
        // Clean install, listener alive.
        assert!(BootReport {
            driver: "rdev",
            chord: "ctrl_l".to_owned(),
            install_ms: 5,
            listener_exited_early: false,
            install_error: None,
        }
        .ok());
        // Install error dominates.
        assert!(!BootReport {
            driver: "none",
            chord: "ctrl_l".to_owned(),
            install_ms: 5,
            listener_exited_early: false,
            install_error: Some("boom".to_owned()),
        }
        .ok());
        // Listener exit dominates when install was Ok.
        assert!(!BootReport {
            driver: "rdev",
            chord: "ctrl_l".to_owned(),
            install_ms: 5,
            listener_exited_early: true,
            install_error: None,
        }
        .ok());
    }

    #[test]
    fn report_to_json_has_stable_keys() {
        let r = BootReport {
            driver: "rdev",
            chord: "ctrl_l+shift_l".to_owned(),
            install_ms: 42,
            listener_exited_early: false,
            install_error: None,
        };
        let parsed: serde_json::Value = serde_json::from_str(&r.to_json()).expect("valid JSON");
        assert_eq!(parsed["kind"], "hotkey_boot_self_test");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["driver"], "rdev");
        assert_eq!(parsed["chord"], "ctrl_l+shift_l");
        assert_eq!(parsed["install_ms"], 42);
        assert_eq!(parsed["listener_exited_early"], false);
        assert!(parsed["install_error"].is_null());
    }

    #[test]
    fn report_to_json_encodes_install_error_as_string() {
        let r = BootReport {
            driver: "none",
            chord: "super_l".to_owned(),
            install_ms: 1,
            listener_exited_early: false,
            install_error: Some("hotkey key name \"super_l\" is not supported by …".to_owned()),
        };
        let parsed: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["driver"], "none");
        assert!(parsed["install_error"]
            .as_str()
            .unwrap()
            .contains("super_l"));
    }

    #[test]
    fn report_to_plain_marks_pass_and_fail_lines_distinctly() {
        let pass = BootReport {
            driver: "rdev",
            chord: "ctrl_l".to_owned(),
            install_ms: 3,
            listener_exited_early: false,
            install_error: None,
        };
        let plain = pass.to_plain();
        assert!(
            plain.starts_with("[self-test hotkey-boot]"),
            "grep prefix: {plain}"
        );
        assert!(plain.contains("driver=rdev"));
        assert!(plain.contains("chord=ctrl_l"));
        assert!(plain.ends_with(" -> PASS"), "PASS marker missing: {plain}");

        let fail = BootReport {
            driver: "none",
            chord: "super_l".to_owned(),
            install_ms: 1,
            listener_exited_early: false,
            install_error: Some("boom".to_owned()),
        };
        let plain = fail.to_plain();
        assert!(
            plain.contains("install_error="),
            "install_error missing: {plain}"
        );
        assert!(plain.ends_with(" -> FAIL"), "FAIL marker missing: {plain}");
    }

    // -----------------------------------------------------------------------
    // Codex P2 #644 discussion r3658983556 — preserve config-load errors.
    //
    // The pre-fix `handle_self_test_hotkey_boot` used
    // `load_settings().map(|s| s.key).unwrap_or_default()`, which
    // silently discarded a corrupt-config I/O or parse error and turned
    // it into the misleading "no PTT chord configured" message an
    // operator would see downstream. The fix routes the load result
    // through `reconcile_config_load` which propagates the Err verbatim
    // whenever the caller did not supply an explicit `--chord` override.
    // -----------------------------------------------------------------------

    #[test]
    fn reconcile_config_load_propagates_error_without_chord_override() {
        // No override + load error: MUST return Err carrying the original
        // failure so the operator sees the config-path root cause. The
        // pre-fix behaviour ate the Err and produced a downstream
        // "no PTT chord configured" message — this assertion FAILS on
        // the un-fixed code because it would return Ok("") instead.
        let err = reconcile_config_load("", Err("corrupt TOML at line 12".to_owned()))
            .expect_err("config-load Err without override must propagate");
        assert!(
            err.contains("corrupt TOML at line 12"),
            "the operator needs to see the original config-load error; \
             got {err:?}"
        );
        assert!(
            err.contains("--chord"),
            "the propagated error must include the workaround hint so the \
             operator learns how to bypass the config lookup: {err:?}"
        );
    }

    #[test]
    fn reconcile_config_load_swallows_error_when_chord_override_supplied() {
        // Override provided: the config's `key` value isn't going to be
        // consulted anyway, so a load failure isn't fatal. Return Ok
        // (with empty string, since the override will replace it in
        // `resolve_chord`).
        let ok = reconcile_config_load("ctrl_l", Err("EACCES".to_owned()))
            .expect("chord override must let the CLI continue past a config-load failure");
        assert_eq!(
            ok, "",
            "the returned config_key is unused when --chord is set; \
             an empty string is the documented placeholder"
        );
        // Whitespace-only override still counts as "no override" so
        // that `--chord \"  \"` on the shell does not accidentally
        // silence a corrupt-config error.
        let err = reconcile_config_load("   ", Err("boom".to_owned()))
            .expect_err("whitespace-only override must NOT bypass config-load error");
        assert!(err.contains("boom"));
    }

    #[test]
    fn reconcile_config_load_returns_config_key_on_ok() {
        // Happy path unchanged: load succeeded, return the key verbatim
        // whether or not an override is supplied (the override selection
        // happens later in resolve_chord).
        assert_eq!(
            reconcile_config_load("", Ok("ctrl_l+shift_l".to_owned())).unwrap(),
            "ctrl_l+shift_l"
        );
        assert_eq!(
            reconcile_config_load("f9", Ok("ctrl_l".to_owned())).unwrap(),
            "ctrl_l",
            "override selection happens in resolve_chord; reconcile just \
             passes the config key through so the callers stay decoupled"
        );
    }

    /// The chord field is present even on install failure so an
    /// operator debugging a bug ticket immediately sees what the
    /// verb tried to install (a mismatch between the operator's
    /// mental model and the actual config is a common cause of
    /// "PTT doesn't fire" reports).
    #[test]
    fn report_carries_chord_even_on_install_failure() {
        let r = BootReport {
            driver: "none",
            chord: "ctrl_l+shift_l".to_owned(),
            install_ms: 0,
            listener_exited_early: false,
            install_error: Some("no display".to_owned()),
        };
        assert_eq!(r.chord, "ctrl_l+shift_l");
        assert!(r.to_json().contains("\"chord\":\"ctrl_l+shift_l\""));
    }
}
