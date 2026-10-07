//! Backend selection and preflight inspection for the Rust hotkey
//! subsystem: driver-kind resolution, chord validation against the
//! selected listener, the preflight query used by doctor/diagnostics,
//! and the `VOICEPI_HOTKEY_BACKEND` env gates.
//!
//! Split out of `hotkey/mod.rs` together with the tests that pin these
//! gates so selection logic and its pins stay in one module.

#[cfg(feature = "rust-hotkeys")]
use super::manager;
#[cfg(feature = "rust-hotkeys")]
use super::manager::{driver_from_env, is_rdev_supported_name, DriverKind};

use super::config::{HotkeyPreflight, InstallError, Result};

/// Resolve the driver kind for a fresh install, applying the Windows-
/// specific `Register → Rdev` chord-validity fallback.
///
/// The chord is parsed by [`manager::win_registerhotkey::parse_chord`]
/// BEFORE any thread spawns, so a modifier-only binding (unsupported
/// by RegisterHotKey) downgrades to rdev with a diagnostic-log line
/// instead of failing the install. The GUI binary defaults to
/// `Register` (set in `whisper-dictate-gui::main`); this fallback
/// exists so a user with a bare-`ctrl_l` binding does NOT lose PTT
/// because the RegisterHotKey backend can't express the chord.
///
/// On non-Windows targets, or for any non-Register selection, this is
/// a straight `driver_from_env()` pass-through.
#[cfg(feature = "rust-hotkeys")]
fn driver_plan_for_kind(key_names: &[String], kind: DriverKind) -> (DriverKind, Option<String>) {
    #[cfg(target_os = "windows")]
    {
        if kind == DriverKind::Register {
            if let Err(msg) = manager::win_registerhotkey::parse_chord(key_names) {
                return (DriverKind::Rdev, Some(msg));
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = key_names; // silence unused-var warning on non-Windows
    }
    (kind, None)
}

#[cfg(feature = "rust-hotkeys")]
pub(super) fn resolve_driver_kind_for_install(key_names: &[String]) -> DriverKind {
    let (kind, fallback_reason) = driver_plan_for_kind(key_names, driver_from_env());
    if let Some(msg) = fallback_reason {
        crate::diag::log!(
            "[hotkey] VOICEPI_HOTKEY_DRIVER=register cannot express \
             the configured chord ({msg}); falling back to rdev for \
             this install so PTT still works (set \
             VOICEPI_HOTKEY_DRIVER=rdev in the environment to make \
             this the pinned default and silence this line)."
        );
    }
    kind
}

/// Inspect the actual session selector and validate the chord without taking
/// push-to-talk ownership or spawning a listener.
#[cfg(feature = "rust-hotkeys")]
pub(super) fn preflight_hotkey_for_kind(
    key_names: &[String],
    selected_driver: DriverKind,
) -> Result<HotkeyPreflight> {
    if key_names.is_empty() {
        return Err(InstallError::EmptyConfig);
    }
    let (driver_kind, fallback_reason) = driver_plan_for_kind(key_names, selected_driver);
    match driver_kind {
        #[cfg(target_os = "windows")]
        DriverKind::Register => {
            manager::win_registerhotkey::parse_chord(key_names)
                .map_err(InstallError::UnsupportedKey)?;
        }
        _ => {
            for name in key_names {
                if !is_rdev_supported_name(name) {
                    return Err(InstallError::UnsupportedKey(name.clone()));
                }
            }
        }
    }
    let planned_driver = manager::driver_label(driver_kind);
    Ok(HotkeyPreflight {
        planned_driver,
        fallback_reason,
        focused_window_risk: cfg!(target_os = "windows") && planned_driver == "rdev",
    })
}

#[cfg(feature = "rust-hotkeys")]
pub fn preflight_hotkey(key_names: &[String]) -> Result<HotkeyPreflight> {
    preflight_hotkey_for_kind(key_names, driver_from_env())
}

/// A reduced build has no listener to preflight, so report the same feature
/// error a real install would return.
#[cfg(not(feature = "rust-hotkeys"))]
pub fn preflight_hotkey(_key_names: &[String]) -> Result<HotkeyPreflight> {
    Err(InstallError::Unsupported)
}

/// Validate `key_names` against the Rust (rdev) backend's supported list
/// WITHOUT installing anything. Mirrors the validation `install_hotkey`
/// runs before spawning manager / coordinator threads, so the supervisor
/// can check a (possibly updated) restart-time binding before replacing
/// the installed listener. Returns Err with the first unsupported name.
///
/// In stub builds (no `rust-hotkeys` feature) this always succeeds --
/// `install_hotkey` already returns `Unsupported` synchronously there,
/// so the supervisor's `hotkey_handle` is None and this validation is
/// dead code anyway.
#[cfg(feature = "rust-hotkeys")]
pub fn validate_key_names(key_names: &[String]) -> Result<()> {
    if key_names.is_empty() {
        return Err(InstallError::EmptyConfig);
    }
    for name in key_names {
        if !is_rdev_supported_name(name) {
            return Err(InstallError::UnsupportedKey(name.clone()));
        }
    }
    Ok(())
}

#[cfg(not(feature = "rust-hotkeys"))]
pub fn validate_key_names(_key_names: &[String]) -> Result<()> {
    Ok(())
}

/// Has the user requested the Rust hotkey backend via env var? Pure helper
/// (no side effects) so the gate is unit-testable without spawning threads.
/// Returns false for unset / empty / any non-`rust` value.
pub fn rust_hotkey_backend_requested() -> bool {
    std::env::var("VOICEPI_HOTKEY_BACKEND")
        .map(|v| v.trim().eq_ignore_ascii_case("rust"))
        .unwrap_or(false)
}

/// Whether the running binary can actually serve the request. The Rust
/// hotkey loop is gated behind the `rust-hotkeys` cargo feature, so a
/// stock build returns false even if the env var is set. The supervisor
/// logs a one-line warning and stays on the pynput path in that case so
/// the user is never silently surprised.
pub fn rust_hotkey_backend_available() -> bool {
    cfg!(feature = "rust-hotkeys")
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
