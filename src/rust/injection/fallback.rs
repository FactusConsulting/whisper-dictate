//! Linux helper-binary fallback chain.
//!
//! `enigo` covers Windows and macOS directly. On Linux the only general-purpose
//! synthetic-input paths are external helpers (Wayland refuses to expose
//! `EvDev`/X test extensions to arbitrary clients). This module picks the right
//! helper for the active session and returns the chain so the dispatcher can
//! try them in order. Selection is pure logic over environment variables and
//! `which` lookups so it can be unit-tested without a display server.
//!
//! Chain rationale (in order, first match wins):
//!
//! * **KDE Wayland** → `kwtype` (KDE's first-party Wayland virtual-keyboard
//!   client; respects KWin's keyboard layout).
//! * **Other Wayland** → `wtype` (community Wayland virtual keyboard, works on
//!   sway/Hyprland/GNOME with the `wlroots`/`virtual-keyboard-v1` protocol).
//! * **Both Wayland and X11 sessions** → `dotool` (newer uinput tool, no daemon)
//!   then `ydotool` (the established uinput tool we already ship, requires the
//!   `ydotoold` socket).
//! * **X11 only** → `xdotool` first (no privileged uinput needed).

use std::env;
use std::path::{Path, PathBuf};

/// The session backend chosen at runtime — feeds [`fallback_chain`] and the
/// log line shown in the worker output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxSession {
    KdeWayland,
    OtherWayland,
    X11,
    Unknown,
}

impl LinuxSession {
    /// Detect the session from the standard XDG/Wayland environment variables.
    pub fn detect() -> Self {
        Self::from_env(|name| env::var(name).ok())
    }

    /// Pure-function variant for tests — the caller supplies the environment
    /// lookup so it doesn't have to mutate process state.
    pub fn from_env<F>(get_env: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let wayland_display = get_env("WAYLAND_DISPLAY");
        let session_type = get_env("XDG_SESSION_TYPE").map(|s| s.to_lowercase());
        let current_desktop = get_env("XDG_CURRENT_DESKTOP")
            .or_else(|| get_env("DESKTOP_SESSION"))
            .map(|s| s.to_lowercase());

        let on_wayland = wayland_display.is_some() || session_type.as_deref() == Some("wayland");
        let on_x11 =
            !on_wayland && (get_env("DISPLAY").is_some() || session_type.as_deref() == Some("x11"));

        if on_wayland {
            let is_kde = current_desktop
                .as_deref()
                .map(|d| d.contains("kde") || d.contains("plasma"))
                .unwrap_or(false);
            if is_kde {
                LinuxSession::KdeWayland
            } else {
                LinuxSession::OtherWayland
            }
        } else if on_x11 {
            LinuxSession::X11
        } else {
            LinuxSession::Unknown
        }
    }
}

/// Names of the helpers in the order the dispatcher should try them.
/// Returned as `&'static str` because the names are baked into the binary.
pub fn fallback_chain(session: LinuxSession) -> &'static [&'static str] {
    match session {
        LinuxSession::KdeWayland => &["kwtype", "wtype", "dotool", "ydotool"],
        LinuxSession::OtherWayland => &["wtype", "dotool", "ydotool"],
        LinuxSession::X11 => &["xdotool", "dotool", "ydotool"],
        // Unknown session — try every helper before giving up.
        LinuxSession::Unknown => &["kwtype", "wtype", "xdotool", "dotool", "ydotool"],
    }
}

/// Walk the chain and return the first helper present on `$PATH`, or `None`
/// when no usable helper is installed. `locator` is injected so unit tests can
/// simulate a sparse install without polluting the process environment.
pub fn select_helper<F>(session: LinuxSession, locator: F) -> Option<&'static str>
where
    F: Fn(&str) -> Option<PathBuf>,
{
    fallback_chain(session)
        .iter()
        .copied()
        .find(|name| locator(name).is_some())
}

/// Default helper locator: `which`-style search across `$PATH`. Tested via the
/// injected variant above; this wrapper only exists for the runtime.
pub fn locate_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        for candidate in candidate_paths(&dir, name) {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn candidate_paths(dir: &Path, name: &str) -> Vec<PathBuf> {
    if cfg!(windows) {
        // Tests on Windows hit this path; honour PATHEXT-style suffixes.
        let mut out = vec![dir.join(name)];
        for ext in ["exe", "bat", "cmd"] {
            out.push(dir.join(format!("{name}.{ext}")));
        }
        out
    } else {
        vec![dir.join(name)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn detects_kde_wayland_session() {
        let session = LinuxSession::from_env(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
        ]));
        assert_eq!(session, LinuxSession::KdeWayland);
    }

    #[test]
    fn detects_kde_wayland_via_plasma_marker() {
        let session = LinuxSession::from_env(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("XDG_CURRENT_DESKTOP", "KDE:plasmawayland"),
        ]));
        assert_eq!(session, LinuxSession::KdeWayland);
    }

    #[test]
    fn detects_other_wayland_session() {
        let session = LinuxSession::from_env(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
        ]));
        assert_eq!(session, LinuxSession::OtherWayland);
    }

    #[test]
    fn detects_x11_when_no_wayland_display() {
        let session =
            LinuxSession::from_env(env_from(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "x11")]));
        assert_eq!(session, LinuxSession::X11);
    }

    #[test]
    fn detects_wayland_via_session_type_alone() {
        // Some sessions don't set WAYLAND_DISPLAY for sub-processes; the
        // XDG_SESSION_TYPE marker alone must still flip us to Wayland.
        let session = LinuxSession::from_env(env_from(&[("XDG_SESSION_TYPE", "wayland")]));
        assert_eq!(session, LinuxSession::OtherWayland);
    }

    #[test]
    fn no_display_markers_yields_unknown() {
        let session = LinuxSession::from_env(env_from(&[]));
        assert_eq!(session, LinuxSession::Unknown);
    }

    #[test]
    fn kde_wayland_chain_starts_with_kwtype() {
        assert_eq!(fallback_chain(LinuxSession::KdeWayland)[0], "kwtype");
    }

    #[test]
    fn x11_chain_starts_with_xdotool() {
        assert_eq!(fallback_chain(LinuxSession::X11)[0], "xdotool");
    }

    #[test]
    fn other_wayland_chain_starts_with_wtype() {
        assert_eq!(fallback_chain(LinuxSession::OtherWayland)[0], "wtype");
    }

    #[test]
    fn select_helper_picks_first_installed() {
        let installed = ["wtype"];
        let locator = |name: &str| {
            if installed.contains(&name) {
                Some(PathBuf::from(format!("/usr/bin/{name}")))
            } else {
                None
            }
        };
        assert_eq!(
            select_helper(LinuxSession::KdeWayland, locator),
            Some("wtype")
        );
    }

    #[test]
    fn select_helper_returns_none_for_empty_install() {
        let locator = |_: &str| None;
        assert!(select_helper(LinuxSession::Unknown, locator).is_none());
    }

    #[test]
    fn select_helper_falls_through_to_ydotool_when_nothing_else_present() {
        let locator = |name: &str| (name == "ydotool").then(|| PathBuf::from("/usr/bin/ydotool"));
        assert_eq!(
            select_helper(LinuxSession::OtherWayland, locator),
            Some("ydotool")
        );
    }

    #[test]
    fn unknown_chain_includes_every_helper() {
        let chain = fallback_chain(LinuxSession::Unknown);
        for name in ["kwtype", "wtype", "xdotool", "dotool", "ydotool"] {
            assert!(chain.contains(&name), "chain missing {name}");
        }
    }
}
