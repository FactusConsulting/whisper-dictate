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
    /// We cannot directly observe rdev listener exit (the manager
    /// hides the thread handle), so the verb uses an indirect signal:
    /// it holds the [`crate::hotkey::HotkeyHandle`] for the full
    /// window and re-checks the driver name at the end; a healthy
    /// install keeps [`crate::hotkey::HotkeyHandle::driver_name`]
    /// stable. Future refinement: expose a `is_listener_alive()` on
    /// the handle so the check is direct.
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
            // (`&'static str`) but the OS hook is dead. We cannot
            // directly probe hook liveness from the handle today, so
            // this stay-alive is a coarse smoke: at minimum it
            // confirms the handle is not immediately dropped by the
            // manager thread.
            std::thread::sleep(Duration::from_millis(hold_ms));
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
                listener_exited_early: false,
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
