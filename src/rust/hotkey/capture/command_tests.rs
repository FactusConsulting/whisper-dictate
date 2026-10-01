use super::*;
use std::io::Cursor;

#[test]
fn configure_confirmation_skips_capture_residue_before_yes() {
    let mut input = Cursor::new("\n\u{1b}[12~\nyes\n");
    let mut output = Vec::new();

    assert!(read_save_confirmation(&mut input, &mut output, true).unwrap());
    assert!(String::from_utf8(output).unwrap().contains("please answer"));
}

#[test]
fn configure_confirmation_uses_enter_as_no_without_capture_residue() {
    let mut input = Cursor::new("\n");
    let mut output = Vec::new();

    assert!(!read_save_confirmation(&mut input, &mut output, false).unwrap());
}

#[test]
fn validate_driver_flag_accepts_canonical_names() {
    // The three canonical values map to the runtime's DriverKind enum.
    assert!(validate_driver_flag("auto").is_ok());
    assert!(validate_driver_flag("rdev").is_ok());
    assert!(validate_driver_flag("evdev").is_ok());
}

#[test]
fn validate_driver_flag_accepts_session_aliases() {
    // Session-name aliases mirror the manager's `DriverKind::parse`
    // acceptance set so a user can type the display server name.
    assert!(validate_driver_flag("x11").is_ok());
    assert!(validate_driver_flag("wayland").is_ok());
}

#[test]
fn validate_driver_flag_is_case_insensitive() {
    assert!(validate_driver_flag("AUTO").is_ok());
    assert!(validate_driver_flag(" Evdev ").is_ok());
}

#[test]
fn validate_driver_flag_rejects_typos_up_front() {
    // CLI-side strictness: unlike the env-var path, `--driver` must
    // fail-fast on a typo so a smoke script sees a clear error instead
    // of silently installing the auto-selected backend.
    let err = validate_driver_flag("uinput").unwrap_err().to_string();
    assert!(
        err.contains("--driver"),
        "error should mention --driver: {err}"
    );
    assert!(
        err.contains("uinput"),
        "error should echo the bad value: {err}"
    );
}

#[test]
fn validate_driver_flag_accepts_register_aliases() {
    // Keep the documented driver aliases parseable in both featureful
    // and stock builds; installation reports missing support later.
    assert!(validate_driver_flag("register").is_ok());
    assert!(validate_driver_flag("win_registerhotkey").is_ok());
    assert!(validate_driver_flag("wm_hotkey").is_ok());
    // Case + whitespace folding mirrors the canonical names.
    assert!(validate_driver_flag(" REGISTER ").is_ok());
    assert!(validate_driver_flag("Win_RegisterHotKey").is_ok());
    assert!(validate_driver_flag("WM_HOTKEY").is_ok());
}

#[test]
fn configure_rejects_json_output() {
    let err = validate_configure_args(true, true, false, None, false, "auto", None)
        .expect_err("interactive capture must reject JSON output")
        .to_string();
    assert!(err.contains("--configure"));
    assert!(err.contains("--json"));
}

#[test]
fn configure_rejects_chord_only_output() {
    let err = validate_configure_args(true, false, true, None, false, "auto", None)
        .expect_err("interactive capture needs raw key events")
        .to_string();
    assert!(err.contains("--chord-events-only"));
}

#[test]
fn focus_process_is_limited_to_chord_only_diagnostics() {
    let err = validate_configure_args(false, false, false, Some(42), false, "auto", None)
        .expect_err("foreground classification is private probe IPC")
        .to_string();
    assert!(err.contains("--focus-process"));
    assert!(validate_configure_args(false, true, true, Some(42), false, "auto", None).is_ok());
}

#[cfg(target_os = "windows")]
#[test]
fn configure_rejects_register_driver() {
    let err = validate_configure_args(true, false, false, None, false, "register", None)
        .expect_err("register driver cannot expose raw key events")
        .to_string();
    assert!(err.contains("raw key-event listener"));
    assert!(err.contains("rdev"));
}

#[test]
fn configure_rejects_chord_override() {
    let err = validate_configure_args(true, false, false, None, false, "auto", Some("ctrl+f9"))
        .expect_err("configure must not silently ignore --chord")
        .to_string();
    assert!(err.contains("--configure"));
    assert!(err.contains("--chord"));
}

#[test]
fn configure_rejects_exit_on_chord() {
    let err = validate_configure_args(true, false, false, None, true, "auto", None)
        .expect_err("release-based capture cannot exit on the first press")
        .to_string();
    assert!(err.contains("--configure"));
    assert!(err.contains("--exit-on-chord"));
}

#[cfg(not(target_os = "windows"))]
#[test]
fn configure_allows_register_alias_for_platform_fallback() {
    assert!(validate_configure_args(true, false, false, None, false, "register", None).is_ok());
}

// -----------------------------------------------------------------------
// counts_as_chord — the producer-side of the Chords: counter
// -----------------------------------------------------------------------
