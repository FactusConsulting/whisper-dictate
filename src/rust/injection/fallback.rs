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

/// Every helper from the session's chain that is present on PATH, in chain
/// order. `paste_capable_only` applies the same dotool exclusion
/// [`select_paste_helper`] documents.
///
/// Both single-pick helpers delegate here so the candidate SET and its order
/// can only be defined once -- a runtime fallback that walked its own list
/// would be free to disagree with the picker about what is eligible.
pub fn available_helpers<F>(
    session: LinuxSession,
    locator: F,
    paste_capable_only: bool,
) -> Vec<&'static str>
where
    F: Fn(&str) -> Option<PathBuf>,
{
    fallback_chain(session)
        .iter()
        .copied()
        .filter(|name| !(paste_capable_only && *name == "dotool"))
        .filter(|name| locator(name).is_some())
        .collect()
}

/// Walk the chain and return the first helper present on `$PATH`, or `None`
/// when no usable helper is installed. `locator` is injected so unit tests can
/// simulate a sparse install without polluting the process environment.
pub fn select_helper<F>(session: LinuxSession, locator: F) -> Option<&'static str>
where
    F: Fn(&str) -> Option<PathBuf>,
{
    available_helpers(session, locator, false).first().copied()
}

/// Like [`select_helper`] but skips helpers that cannot perform a paste
/// shortcut even if installed. Today that is just `dotool`: its
/// command-line surface doesn't expose the modifier-aware key chord that
/// `wtype`/`kwtype`/`xdotool`/`ydotool` use for paste (see
/// `linux_helpers::shortcut_to_helper_chord` which returns an error for
/// `("dotool", _)`). Picking dotool for paste therefore reliably fails at
/// the chord-builder layer — selecting the NEXT helper up front avoids
/// the wasted spawn and gives the user the helper that actually works.
///
/// P3 #371 finding 1: dotool was eligible in `select_helper`, so a host
/// with only `dotool` + `ydotool` installed would always pick dotool
/// first for KDE/Other-Wayland chains and immediately error; this picker
/// skips dotool so ydotool wins, which DOES have paste support via
/// `wayland::paste_shortcut`.
pub fn select_paste_helper<F>(session: LinuxSession, locator: F) -> Option<&'static str>
where
    F: Fn(&str) -> Option<PathBuf>,
{
    available_helpers(session, locator, true).first().copied()
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

    // -- P3 #371 finding 1: select_paste_helper skips dotool ---------------

    #[test]
    fn select_paste_helper_skips_dotool_even_when_installed() {
        // dotool's paste chord is not implemented (shortcut_to_helper_chord
        // returns Err for it), so selecting it for paste guarantees an
        // immediate failure. The paste-aware selector must skip it and
        // pick the next eligible helper.
        let installed = ["dotool", "ydotool"];
        let locator = |name: &str| {
            installed
                .contains(&name)
                .then(|| PathBuf::from(format!("/usr/bin/{name}")))
        };
        assert_eq!(
            select_paste_helper(LinuxSession::KdeWayland, locator),
            Some("ydotool"),
            "with dotool+ydotool installed, paste must pick ydotool — dotool has no paste chord"
        );
    }

    #[test]
    fn select_paste_helper_returns_none_when_only_dotool_present() {
        // The whole point of the dotool exclusion: if dotool is the ONLY
        // installed helper, the paste path must surface "no paste helper
        // installed" cleanly rather than picking dotool and failing at
        // the chord builder.
        let locator = |name: &str| (name == "dotool").then(|| PathBuf::from("/usr/bin/dotool"));
        assert_eq!(
            select_paste_helper(LinuxSession::KdeWayland, locator),
            None,
            "dotool alone must not be eligible for paste"
        );
    }

    #[test]
    fn select_paste_helper_matches_select_helper_when_dotool_absent() {
        // Belt-and-braces: when dotool is not installed, the paste selector
        // must behave identically to the typing selector — same chain,
        // same first hit.
        let installed = ["wtype", "ydotool"];
        let locator = |name: &str| {
            installed
                .contains(&name)
                .then(|| PathBuf::from(format!("/usr/bin/{name}")))
        };
        for session in [
            LinuxSession::KdeWayland,
            LinuxSession::OtherWayland,
            LinuxSession::X11,
            LinuxSession::Unknown,
        ] {
            assert_eq!(
                select_paste_helper(session, locator),
                select_helper(session, locator),
                "without dotool, select_paste_helper must mirror select_helper for {session:?}"
            );
        }
    }

    #[test]
    fn select_paste_helper_prefers_kwtype_over_dotool_on_kde() {
        // Direct regression: KDE chain is [kwtype, wtype, dotool, ydotool].
        // With kwtype and dotool installed, paste must pick kwtype (not
        // dotool which appears earlier on the chain than ydotool).
        let installed = ["kwtype", "dotool"];
        let locator = |name: &str| {
            installed
                .contains(&name)
                .then(|| PathBuf::from(format!("/usr/bin/{name}")))
        };
        assert_eq!(
            select_paste_helper(LinuxSession::KdeWayland, locator),
            Some("kwtype")
        );
    }
}

/// Whether a failed helper invocation is safe to follow with the NEXT helper
/// in the chain.
///
/// This gate is the whole risk of a runtime fallback. If a helper typed half
/// the transcript and then died, retrying would type the other helper's full
/// text on top of the partial one -- duplicating the user's dictation into
/// whatever document they were writing. Losing an utterance is annoying;
/// silently corrupting a document is worse. So the rule is deliberately
/// conservative: retry ONLY on failures that prove nothing reached the
/// compositor, and surface anything unrecognised as-is.
///
/// The recognised signatures are all startup / capability failures, i.e. the
/// helper refused before emitting a single keystroke:
///
///   - `wtype` on a compositor without `zwp_virtual_keyboard_v1` (KWin --
///     the case that motivated this),
///   - a helper that is on PATH but cannot execute (spawn errors),
///   - X11 helpers with no reachable display,
///   - `ydotool` when `ydotoold` is not running / its socket is unusable.
///
/// Error returned from a single helper attempt in the runtime fallback
/// chain (`dispatcher::try_helpers`). Carries both the underlying error and
/// a flag that indicates whether the helper had already pushed one or more
/// keystrokes to the compositor when it failed.
///
/// `partial: true` disables fallback -- the next helper would re-type the
/// successful prefix on top of what the first helper already injected,
/// silently double-typing part of the transcript into the user's document.
/// Losing an utterance is annoying; corrupting a document is worse.
///
/// Two producers exist:
///
/// * The evdev-driven [`super::wayland::type_text_tracked`] path splits an
///   injection into multiple `ydotool` calls and DOES observe partial
///   progress; it stamps `partial: true` whenever any op succeeded before
///   the failure.
/// * Subprocess helpers (`kwtype` / `wtype` / `xdotool` / `dotool` as a
///   single opaque child process) cannot see partial progress; they build a
///   `HelperError` via [`HelperError::opaque`] and rely on
///   [`is_safe_to_try_next_helper`] as the safety gate -- a conservative
///   text-match against KNOWN startup / capability signatures. Anything
///   unrecognised stops the chain, matching the pre-tracking behaviour.
pub struct HelperError {
    pub err: anyhow::Error,
    pub partial: bool,
    /// `true` iff the caller can PROVE no key operations reached the
    /// compositor. Distinct from `partial: false`, which only means
    /// "unknown". The dispatcher uses this to keep the outer-fallback
    /// `partial=true` stamp off failures where we actually know nothing
    /// landed (Codex P2 #636 dispatcher.rs:708). The dominant producer
    /// is the ydotool evdev-tracked path: `sent == 0` on a failed
    /// invocation proves nothing was injected, and Python's outer
    /// fallback must be free to retry.
    pub known_no_progress: bool,
}

impl HelperError {
    /// Failure that the helper cannot classify as pre- or post-first-key.
    /// The dispatcher will fall back to text-matching on the error string
    /// via [`is_safe_to_try_next_helper`]. Only use this for subprocess
    /// helpers whose partial state is unobservable from Rust.
    pub fn opaque(err: anyhow::Error) -> Self {
        Self {
            err,
            partial: false,
            known_no_progress: false,
        }
    }

    /// Failure AFTER at least one keystroke reached the compositor. The
    /// dispatcher MUST NOT try the next helper -- doing so would re-type
    /// the already-injected prefix.
    pub fn partial(err: anyhow::Error) -> Self {
        Self {
            err,
            partial: true,
            known_no_progress: false,
        }
    }

    /// Failure with a positive proof that no key operations reached the
    /// compositor (e.g. ydotool `sent == 0`). Behaves like [`Self::opaque`]
    /// for the safe-to-retry decision, but tells the dispatcher not to
    /// stamp `partial=true` in the `idx > 0` opaque-failure branch —
    /// Python's outer fallback must be free to re-type the transcript.
    /// Codex P2 #636 dispatcher.rs:708.
    pub fn none_landed(err: anyhow::Error) -> Self {
        Self {
            err,
            partial: false,
            known_no_progress: true,
        }
    }
}

pub fn is_safe_to_try_next_helper(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    const STARTUP_FAILURES: &[&str] = &[
        // wtype / kwtype: compositor lacks the virtual-keyboard protocol.
        "does not support",
        "no such interface",
        // Spawn failures (binary on PATH but unusable: wrong arch, noexec
        // mount, missing loader).
        "no such file or directory",
        "permission denied",
        "exec format error",
        // X11 helpers without a display. `xdotool` really prints
        // `Error: Can't open display: (null)` (with an apostrophe) --
        // the previous `cannot open display` matcher never fired for
        // the exact tool it was meant to catch. Keep the other spellings
        // for kwtype/wtype/dotool cross-tool defence.
        "can't open display",
        "cannot open display",
        "unable to open display",
        "failed to connect to display",
        // ydotool without a running ydotoold.
        "failed to connect socket",
        "connection refused",
        "socket file",
    ];
    STARTUP_FAILURES.iter().any(|sig| lower.contains(sig))
}

#[cfg(test)]
mod runtime_fallback_tests {
    use super::*;

    fn present(names: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |n: &str| {
            names
                .contains(&n)
                .then(|| PathBuf::from(format!("/usr/bin/{n}")))
        }
    }

    #[test]
    fn available_helpers_keeps_chain_order_and_drops_absent() {
        // KDE chain is kwtype, wtype, dotool, ydotool.
        let got = available_helpers(
            LinuxSession::KdeWayland,
            present(&["ydotool", "wtype"]),
            false,
        );
        assert_eq!(got, vec!["wtype", "ydotool"]);
    }

    #[test]
    fn available_helpers_excludes_dotool_for_paste_only() {
        let installed = present(&["dotool", "ydotool"]);
        assert_eq!(
            available_helpers(LinuxSession::OtherWayland, &installed, false),
            vec!["dotool", "ydotool"]
        );
        assert_eq!(
            available_helpers(LinuxSession::OtherWayland, &installed, true),
            vec!["ydotool"]
        );
    }

    #[test]
    fn single_pickers_agree_with_the_list_they_delegate_to() {
        // The whole point of routing both through `available_helpers`: the
        // runtime fallback and the single-shot pickers cannot drift.
        for session in [
            LinuxSession::KdeWayland,
            LinuxSession::OtherWayland,
            LinuxSession::X11,
            LinuxSession::Unknown,
        ] {
            let installed = present(&["dotool", "ydotool", "wtype", "xdotool"]);
            assert_eq!(
                select_helper(session, &installed),
                available_helpers(session, &installed, false)
                    .first()
                    .copied()
            );
            assert_eq!(
                select_paste_helper(session, &installed),
                available_helpers(session, &installed, true)
                    .first()
                    .copied()
            );
        }
    }

    #[test]
    fn wtype_on_kwin_is_safe_to_follow_with_the_next_helper() {
        // The exact message from the KDE Plasma box that motivated this.
        assert!(is_safe_to_try_next_helper(
            "wtype type failed: Compositor does not support the virtual keyboard protocol"
        ));
    }

    #[test]
    fn spawn_and_display_failures_are_safe_to_retry() {
        for msg in [
            "No such file or directory (os error 2)",
            "Permission denied (os error 13)",
            // The exact stderr xdotool prints when $DISPLAY is unset --
            // regression pin so anyone who normalises the STARTUP_FAILURES
            // list can't silently drop the real xdotool string. Note the
            // apostrophe -- `cannot open display` would NOT catch this.
            "xdotool type failed: Error: Can't open display: (null)",
            "ydotool: failed to connect socket: No such file or directory",
        ] {
            assert!(is_safe_to_try_next_helper(msg), "should retry after: {msg}");
        }
    }

    #[test]
    fn helper_error_variants_carry_the_expected_flags() {
        // Pin the three constructors so nobody accidentally adds a
        // fourth without updating the dispatcher.
        let o = HelperError::opaque(anyhow::anyhow!("x"));
        assert!(!o.partial && !o.known_no_progress);
        let p = HelperError::partial(anyhow::anyhow!("x"));
        assert!(p.partial && !p.known_no_progress);
        let n = HelperError::none_landed(anyhow::anyhow!("x"));
        assert!(
            !n.partial && n.known_no_progress,
            "none_landed proves no progress reached the compositor \
             (Codex P2 #636 dispatcher.rs:708)"
        );
    }

    #[test]
    fn unrecognised_failures_are_not_retried() {
        // The safety property: anything that might have typed PART of the
        // text must stop the chain, or the next helper types the whole
        // transcript again on top of it.
        for msg in [
            "wtype type failed: killed by signal 9",
            "ydotool type failed: broken pipe after 12 keystrokes",
            "xdotool type failed: X Error of failed request: BadWindow",
            "some entirely novel failure",
        ] {
            assert!(
                !is_safe_to_try_next_helper(msg),
                "must NOT retry after: {msg}"
            );
        }
    }
}
