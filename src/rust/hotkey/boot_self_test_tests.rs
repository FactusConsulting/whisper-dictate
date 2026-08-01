//! Companion tests for [`crate::hotkey::boot_self_test`].
//!
//! Extracted from an inline `#[cfg(test)] mod tests` in `boot_self_test.rs`
//! so the regression-test discipline scanner (per AGENTS.md
//! `enforce-regression-test-discipline` — see
//! `src/tests/python/test_regression_test_discipline.py`) sees a matching
//! test file next to the production module. The inline layout was not
//! picked up by the scanner's "already-tested" exemption, which resolves
//! `foo.rs` → `foo_tests.rs` on the file system. When the sonar quality
//! gate flagged `reconcile_config_load` as an untested new public symbol
//! ( sweep for #644), the tests moved here to satisfy the scanner
//! while keeping the same coverage.

#![cfg(test)]

use super::boot_self_test::{features_available, reconcile_config_load, resolve_chord, BootReport};

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
// #644  r3658983556 — preserve config-load errors.
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
