//! Per-OS accessibility / input-monitoring permissions guide (Issue #328).
//!
//! Whisper-dictate needs three OS-level capabilities that are behind privileged
//! toggles on modern operating systems:
//!
//! - **Microphone access** — required to record audio (macOS: Privacy →
//!   Microphone; Windows: Settings → Privacy → Microphone; Linux: PulseAudio /
//!   PipeWire is unprivileged, X11/Wayland session-only).
//! - **Global hotkey monitoring** — the push-to-talk key must fire while the
//!   focus is in another app (macOS: Input Monitoring; Windows: usually free
//!   except when UAC-elevated targets are focused; Linux/X11: works; Wayland:
//!   compositor-dependent — usually needs a portal).
//! - **Keystroke injection** — the transcription must type into the focused
//!   text field (macOS: Accessibility; Windows: free; Linux/X11: works via
//!   XTEST; Wayland: portal or `wtype`).
//!
//! This module is a **pure** static-data catalog: it maps an [`OsTarget`] to a
//! list of [`PermissionStep`] entries. No I/O, no egui — the UI in
//! `steps.rs` just walks the list and renders it. All strings are English
//! (localization out of scope per the issue).
//!
//! Detection is done via `cfg!(target_os = …)` at call time; the enum is
//! `pub(in crate::ui)` for construction, and tests exercise each variant.

use std::borrow::Cow;

/// Which OS's permission guide to render. The default is derived from
/// `cfg!(target_os = …)` at runtime via [`OsTarget::current`], but the setup
/// wizard also lets the user preview the other OSes (useful when preparing a
/// remote install for a coworker).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsTarget {
    Windows,
    MacOs,
    LinuxX11,
    LinuxWayland,
}

impl OsTarget {
    /// Pick the guide that matches the running OS. Linux picks the display
    /// server via `$WAYLAND_DISPLAY` (present ⇒ Wayland), else X11.
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            OsTarget::Windows
        } else if cfg!(target_os = "macos") {
            OsTarget::MacOs
        } else if wayland_session_present() {
            OsTarget::LinuxWayland
        } else {
            OsTarget::LinuxX11
        }
    }

    /// Short human-readable name used as the guide heading.
    pub fn label(self) -> &'static str {
        match self {
            OsTarget::Windows => "Windows",
            OsTarget::MacOs => "macOS",
            OsTarget::LinuxX11 => "Linux (X11)",
            OsTarget::LinuxWayland => "Linux (Wayland)",
        }
    }
}

/// One concrete permission or capability the user has to grant, together with
/// the human-readable path to the setting and (optionally) a deep-link URL /
/// scheme the wizard can offer as a one-click open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionStep {
    /// Short heading, e.g. "Microphone access".
    pub title: &'static str,
    /// Prose telling the user what to click, click-by-click. Rendered as a
    /// multi-line body; keep lines short so it wraps well in the wizard.
    pub body: Cow<'static, str>,
    /// Optional deep link (e.g. `x-apple.systempreferences:` on macOS, or the
    /// `ms-settings:` URI on Windows). `None` means "no direct deep-link —
    /// user has to navigate manually".
    pub deep_link: Option<&'static str>,
    /// What happens if the user skips this step. Kept short — one sentence.
    /// Rendered as muted footer text under the body.
    pub skip_consequence: &'static str,
}

/// Return the full guide for the given OS target. Ordered "cheapest first" so
/// the wizard walks the user from the least-friction capability (microphone,
/// usually already on) to the most-friction one (accessibility on macOS).
pub fn guide_for(target: OsTarget) -> Vec<PermissionStep> {
    match target {
        OsTarget::Windows => windows_guide(),
        OsTarget::MacOs => macos_guide(),
        OsTarget::LinuxX11 => linux_x11_guide(),
        OsTarget::LinuxWayland => linux_wayland_guide(),
    }
}

fn windows_guide() -> Vec<PermissionStep> {
    vec![
        PermissionStep {
            title: "Microphone access",
            body: Cow::Borrowed(
                "Open Settings → Privacy & security → Microphone. Turn ON \
                 \u{201C}Microphone access\u{201D}, then scroll down and make \
                 sure whisper-dictate (or your terminal, if you launched it \
                 from one) is allowed.",
            ),
            deep_link: Some("ms-settings:privacy-microphone"),
            skip_consequence: "The worker will start but record silence.",
        },
        PermissionStep {
            title: "Global hotkey (push-to-talk)",
            body: Cow::Borrowed(
                "Windows lets any app read the keyboard by default, so no extra \
                 permission is needed. If push-to-talk stops working while an \
                 elevated app is focused (e.g. Task Manager), run whisper-dictate \
                 as administrator or use the toggle-mode hotkey instead.",
            ),
            deep_link: None,
            skip_consequence: "Push-to-talk may not work over UAC-elevated apps.",
        },
        PermissionStep {
            title: "Keystroke injection",
            body: Cow::Borrowed(
                "No configuration required on Windows \u{2014} whisper-dictate uses \
                 the SendInput API which works out of the box.",
            ),
            deep_link: None,
            skip_consequence: "This step has no toggle to skip.",
        },
    ]
}

fn macos_guide() -> Vec<PermissionStep> {
    vec![
        PermissionStep {
            title: "Microphone access",
            body: Cow::Borrowed(
                "Open System Settings \u{2192} Privacy & Security \u{2192} \
                 Microphone. Enable the toggle next to whisper-dictate (or the \
                 terminal you launched it from). You will be prompted the first \
                 time you press push-to-talk.",
            ),
            deep_link: Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
            ),
            skip_consequence: "The worker will start but record silence.",
        },
        PermissionStep {
            title: "Input Monitoring (push-to-talk)",
            body: Cow::Borrowed(
                "Open System Settings \u{2192} Privacy & Security \u{2192} \
                 Input Monitoring. Add whisper-dictate to the list and enable \
                 the toggle. macOS needs this to see your push-to-talk key while \
                 another app is focused.",
            ),
            deep_link: Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
            ),
            skip_consequence: "Push-to-talk only fires while whisper-dictate is focused.",
        },
        PermissionStep {
            title: "Accessibility (keystroke injection)",
            body: Cow::Borrowed(
                "Open System Settings \u{2192} Privacy & Security \u{2192} \
                 Accessibility. Add whisper-dictate to the list and enable the \
                 toggle. Without this, the transcription cannot be typed into \
                 the focused app.",
            ),
            deep_link: Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            ),
            skip_consequence: "Transcriptions are only printed to the log \u{2014} not typed.",
        },
    ]
}

fn linux_x11_guide() -> Vec<PermissionStep> {
    vec![
        PermissionStep {
            title: "Microphone access",
            body: Cow::Borrowed(
                "Verify the mic in `pavucontrol` (or your desktop\u{2019}s sound \
                 settings): switch to the \u{201C}Recording\u{201D} tab, press \
                 push-to-talk, and confirm you see whisper-dictate listed with \
                 a moving level bar. PulseAudio/PipeWire is unprivileged; no OS \
                 toggle is required.",
            ),
            deep_link: None,
            skip_consequence: "The worker records from the wrong device.",
        },
        PermissionStep {
            title: "Global hotkey (X11)",
            body: Cow::Borrowed(
                "X11 lets any client read the raw key stream, so the push-to-talk \
                 hotkey works out of the box. If nothing happens, check that your \
                 desktop\u{2019}s hotkey daemon isn\u{2019}t swallowing the key.",
            ),
            deep_link: None,
            skip_consequence: "Push-to-talk won\u{2019}t reach the worker.",
        },
        PermissionStep {
            title: "Keystroke injection (X11)",
            body: Cow::Borrowed(
                "whisper-dictate uses the XTEST extension; make sure `xdotool` or \
                 the equivalent library is available on `$PATH`. Set the correct \
                 layout in the Speech tab if a non-US keymap is active.",
            ),
            deep_link: None,
            skip_consequence: "Transcription cannot be injected \u{2014} log-only.",
        },
    ]
}

fn linux_wayland_guide() -> Vec<PermissionStep> {
    vec![
        PermissionStep {
            title: "Microphone access",
            body: Cow::Borrowed(
                "As on X11, PulseAudio/PipeWire runs in your user session and \
                 needs no OS toggle. If nothing is captured, verify the input \
                 device in `pavucontrol`.",
            ),
            deep_link: None,
            skip_consequence: "The worker records from the wrong device.",
        },
        PermissionStep {
            title: "Global hotkey (Wayland)",
            body: Cow::Borrowed(
                "Wayland compositors hide the key stream from unfocused apps by \
                 default. On GNOME/KDE, whisper-dictate uses the \
                 xdg-desktop-portal GlobalShortcuts portal \u{2014} approve the \
                 permission dialog the first time push-to-talk is bound. On \
                 tiling compositors (sway, Hyprland) you may need to bind the \
                 key at the compositor level and forward it via a helper.",
            ),
            deep_link: None,
            skip_consequence: "Push-to-talk only works while whisper-dictate is focused.",
        },
        PermissionStep {
            title: "Keystroke injection (Wayland)",
            body: Cow::Borrowed(
                "Install `wtype` (or `ydotool` under a running daemon) so \
                 whisper-dictate can synthesise keystrokes into the focused \
                 window. Without it, only the log receives the transcription.",
            ),
            deep_link: None,
            skip_consequence: "Transcription cannot be injected \u{2014} log-only.",
        },
    ]
}

fn wayland_session_present() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_os_target_returns_a_non_empty_guide() {
        // Every catalog entry the wizard can render must have at least one
        // step. Missing platforms would be caught at compile time (match
        // exhaustive), but an empty list would only surface at first launch.
        for target in [
            OsTarget::Windows,
            OsTarget::MacOs,
            OsTarget::LinuxX11,
            OsTarget::LinuxWayland,
        ] {
            let steps = guide_for(target);
            assert!(
                !steps.is_empty(),
                "OS target {target:?} must have at least one permission step"
            );
            for step in &steps {
                assert!(
                    !step.title.trim().is_empty(),
                    "step title must not be blank ({target:?})",
                );
                assert!(
                    !step.body.trim().is_empty(),
                    "step body must not be blank ({target:?} / {})",
                    step.title,
                );
                assert!(
                    !step.skip_consequence.trim().is_empty(),
                    "each step must document what happens if skipped ({target:?} / {})",
                    step.title,
                );
            }
        }
    }

    #[test]
    fn macos_guide_covers_the_three_privileged_capabilities() {
        // On macOS the three toggles are non-negotiable — without any one of
        // them, the user gets the "nothing happened" first impression.
        let steps = guide_for(OsTarget::MacOs);
        let titles: Vec<&str> = steps.iter().map(|s| s.title).collect();
        assert!(
            titles.iter().any(|t| t.contains("Microphone")),
            "macOS guide missing Microphone step: {titles:?}"
        );
        assert!(
            titles.iter().any(|t| t.contains("Input Monitoring")),
            "macOS guide missing Input Monitoring step: {titles:?}"
        );
        assert!(
            titles.iter().any(|t| t.contains("Accessibility")),
            "macOS guide missing Accessibility step: {titles:?}"
        );
    }

    #[test]
    fn macos_deep_links_use_the_system_preferences_scheme() {
        // The deep-links have to be recognised by System Settings.app; if
        // someone accidentally drops the `x-apple.systempreferences:` prefix,
        // the "Open Settings" button silently fails. Keep the check narrow —
        // only assert the scheme, not the exact anchor.
        for step in guide_for(OsTarget::MacOs) {
            if let Some(link) = step.deep_link {
                assert!(
                    link.starts_with("x-apple.systempreferences:"),
                    "macOS deep-link must use the System Settings scheme: {link:?}"
                );
            }
        }
    }

    #[test]
    fn windows_guide_offers_a_settings_deep_link_for_microphone() {
        // The microphone toggle is the most common source of the "silent
        // recording" bug on Windows; the deep-link is the whole point.
        let steps = guide_for(OsTarget::Windows);
        let mic = steps
            .iter()
            .find(|s| s.title.contains("Microphone"))
            .expect("windows guide must list microphone");
        assert_eq!(
            mic.deep_link,
            Some("ms-settings:privacy-microphone"),
            "microphone deep-link changed \u{2014} update the ms-settings URI"
        );
    }

    #[test]
    fn linux_variants_differ_on_the_hotkey_capability() {
        // The whole point of splitting X11 and Wayland is the hotkey story —
        // that's where the user experience diverges most.
        let x11_hotkey = guide_for(OsTarget::LinuxX11)
            .into_iter()
            .find(|s| s.title.contains("hotkey"))
            .expect("X11 guide must mention hotkeys");
        let wl_hotkey = guide_for(OsTarget::LinuxWayland)
            .into_iter()
            .find(|s| s.title.contains("hotkey"))
            .expect("Wayland guide must mention hotkeys");
        assert_ne!(
            x11_hotkey.body, wl_hotkey.body,
            "X11 and Wayland hotkey guides must not be identical",
        );
    }

    #[test]
    fn os_target_current_matches_cfg_or_wayland_env() {
        // On CI/test hosts the exact answer varies, but `current()` must
        // never panic and must return a variant the guide catalog handles.
        // On Linux the answer depends on WAYLAND_DISPLAY.
        let current = OsTarget::current();
        // Match arm coverage is what actually matters.
        let steps = guide_for(current);
        assert!(!steps.is_empty(), "current() must map to a real guide");
    }
}
