//! Wayland `ydotool` path: evdev key sequence construction + invocation.
//!
//! Carries the original "Phase 1" Wayland injector: the per-layout keymap from
//! [`super::keymap`] is the data source, this module turns text into a series
//! of `ydotool type` / `ydotool key` operations and shells out.
//!
//! Extracted from the legacy `injection.rs`; pure-logic helpers
//! ([`build_ydotool_ops`], [`paste_shortcut_args`], [`target_prefers_terminal_paste`])
//! stay public so unit tests cover them without going near `ydotool` itself.

use std::process::Command;

use anyhow::{anyhow, Result};

use super::keymap::keycodes_for;

/// Synthetic key-up tokens used to drain a held PTT chord before pasting.
/// Mirrors the Python list in `vp_inject.py`; KEY_LEFT/RIGHT for each side of
/// every common modifier so a still-down chord can't turn Ctrl+V into a
/// Ctrl+Shift shortcut by accident.
pub const WAYLAND_MODIFIER_RELEASES: &[&str] = &[
    "29:0",  // KEY_LEFTCTRL
    "97:0",  // KEY_RIGHTCTRL
    "42:0",  // KEY_LEFTSHIFT
    "54:0",  // KEY_RIGHTSHIFT
    "56:0",  // KEY_LEFTALT
    "100:0", // KEY_RIGHTALT
    "125:0", // KEY_LEFTMETA
    "126:0", // KEY_RIGHTMETA
];
pub const WAYLAND_CTRL_V: &[&str] = &["29:1", "47:1", "47:0", "29:0"];
pub const WAYLAND_CTRL_SHIFT_V: &[&str] = &["29:1", "42:1", "47:1", "47:0", "42:0", "29:0"];
/// `Shift+Insert` evdev sequence (terminal "plain text paste" on X11 / many
/// GTK widgets). KEY_LEFTSHIFT=42, KEY_INSERT=110.
pub const WAYLAND_SHIFT_INSERT: &[&str] = &["42:1", "110:1", "110:0", "42:0"];
/// `Super+V` / `Meta+V` evdev sequence (macOS Cmd+V on cross-platform apps,
/// `xremap`-style users). KEY_LEFTMETA=125, KEY_V=47.
pub const WAYLAND_CMD_V: &[&str] = &["125:1", "47:1", "47:0", "125:0"];
pub const LINUX_TERMINAL_TARGETS: &[&str] = &[
    "terminal",
    "ptyxis",
    "kgx",
    "konsole",
    "xterm",
    "alacritty",
    "wezterm",
    "ghostty",
    "kitty",
    "tilix",
    "gnome-console",
    "gnome-terminal",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YdotoolOp {
    Type(String),
    Key(Vec<String>),
}

/// Walk `text` left-to-right and split into runs of layout-mapped key chords
/// (typed verbatim via `ydotool key`) and Unicode chunks (`ydotool type`).
pub fn build_ydotool_ops(text: &str, xkb_layout: &str) -> Vec<YdotoolOp> {
    let mut ops = Vec::new();
    let mut buffer = String::new();
    for ch in text.chars() {
        if let Some(keys) = keycodes_for(xkb_layout, ch) {
            if !buffer.is_empty() {
                ops.push(YdotoolOp::Type(std::mem::take(&mut buffer)));
            }
            ops.push(YdotoolOp::Key(keys));
        } else {
            buffer.push(ch);
        }
    }
    if !buffer.is_empty() {
        ops.push(YdotoolOp::Type(buffer));
    }
    ops
}

/// Heuristic: should we send `Ctrl+Shift+V` (terminal paste) instead of plain
/// `Ctrl+V`? Native Wayland windows are often unidentifiable, so we lean
/// terminal-side when the target is unknown and rely on text widgets to accept
/// `Ctrl+Shift+V` as plain-text paste.
pub fn target_prefers_terminal_paste(target_title: &str, target_process: &str) -> bool {
    let target = format!("{target_title} {target_process}").to_lowercase();
    if target.trim().is_empty() {
        return true;
    }
    LINUX_TERMINAL_TARGETS
        .iter()
        .any(|term| target.contains(term))
}

/// Assemble the `ydotool key` argument vector for the paste shortcut, with the
/// PTT-chord release prelude already prepended.
///
/// This is the LEGACY entry point that always runs the terminal-target
/// heuristic — kept for callers that genuinely have no explicit shortcut
/// to pin (today only the test surface; the dispatcher uses
/// [`paste_shortcut_args_for`]).
pub fn paste_shortcut_args(target_title: &str, target_process: &str) -> Vec<&'static str> {
    let mut args = Vec::with_capacity(WAYLAND_MODIFIER_RELEASES.len() + WAYLAND_CTRL_SHIFT_V.len());
    args.extend_from_slice(WAYLAND_MODIFIER_RELEASES);
    if target_prefers_terminal_paste(target_title, target_process) {
        args.extend_from_slice(WAYLAND_CTRL_SHIFT_V);
    } else {
        args.extend_from_slice(WAYLAND_CTRL_V);
    }
    args
}

/// Assemble the `ydotool key` argument vector for a paste shortcut, honouring
/// an explicit [`super::paste::PasteShortcut`] when given. When `shortcut`
/// is `None`, the terminal-target heuristic decides between Ctrl+V and
/// Ctrl+Shift+V (matching [`paste_shortcut_args`]). When `shortcut` is
/// `Some(x)`, the explicit chord wins regardless of the target — closing
/// the P2 #391 ydotool-path gap where an explicit `Some(CtrlV)` was
/// silently downgraded by the terminal heuristic.
///
/// The PTT-chord release prelude ([`WAYLAND_MODIFIER_RELEASES`]) is always
/// prepended so a still-held PTT modifier cannot accidentally turn the
/// chord into a different shortcut.
pub fn paste_shortcut_args_for(
    shortcut: Option<super::paste::PasteShortcut>,
    target_title: &str,
    target_process: &str,
) -> Vec<&'static str> {
    let chord = paste_shortcut_chord(shortcut, target_title, target_process);
    let mut args = Vec::with_capacity(WAYLAND_MODIFIER_RELEASES.len() + chord.len());
    args.extend_from_slice(WAYLAND_MODIFIER_RELEASES);
    args.extend_from_slice(chord);
    args
}

/// Pure helper: pick the evdev-code slice for `shortcut`, falling back to
/// the terminal-target heuristic when `shortcut` is `None`. Split out so
/// the resolution is unit-testable without touching the release prelude
/// or the `ydotool` command.
fn paste_shortcut_chord(
    shortcut: Option<super::paste::PasteShortcut>,
    target_title: &str,
    target_process: &str,
) -> &'static [&'static str] {
    use super::paste::PasteShortcut;
    match shortcut {
        Some(PasteShortcut::CtrlV) => WAYLAND_CTRL_V,
        Some(PasteShortcut::CtrlShiftV) => WAYLAND_CTRL_SHIFT_V,
        Some(PasteShortcut::ShiftInsert) => WAYLAND_SHIFT_INSERT,
        Some(PasteShortcut::CmdV) => WAYLAND_CMD_V,
        None => {
            if target_prefers_terminal_paste(target_title, target_process) {
                WAYLAND_CTRL_SHIFT_V
            } else {
                WAYLAND_CTRL_V
            }
        }
    }
}

pub fn type_text(text: &str, xkb_layout: &str) -> Result<()> {
    for op in build_ydotool_ops(text, xkb_layout) {
        match op {
            YdotoolOp::Type(chunk) => run_ydotool(["type", "--", &chunk])?,
            YdotoolOp::Key(keys) => {
                let mut args = vec!["key".to_owned()];
                args.extend(keys);
                run_ydotool(args.iter().map(String::as_str))?;
            }
        }
    }
    Ok(())
}

pub fn paste_shortcut(target_title: &str, target_process: &str) -> Result<()> {
    run_ydotool(std::iter::once("key").chain(paste_shortcut_args(target_title, target_process)))
}

/// Run a `ydotool key` paste invocation that honours an explicit
/// [`super::paste::PasteShortcut`]. Closes the P2 #391 gap where the
/// ydotool path of the Linux dispatcher silently dropped the caller's
/// shortcut and re-ran the terminal-target heuristic.
pub fn paste_shortcut_for(
    shortcut: Option<super::paste::PasteShortcut>,
    target_title: &str,
    target_process: &str,
) -> Result<()> {
    run_ydotool(std::iter::once("key").chain(paste_shortcut_args_for(
        shortcut,
        target_title,
        target_process,
    )))
}

fn run_ydotool<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let args: Vec<&str> = args.into_iter().collect();
    let output = Command::new("ydotool").args(&args).output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow!(
        "ydotool {} failed: {}",
        args.join(" "),
        stderr.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_codes(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_owned()).collect()
    }

    #[test]
    fn dk_direct_text_splits_special_chars() {
        assert_eq!(
            build_ydotool_ops("høre", "dk"),
            vec![
                YdotoolOp::Type("h".to_owned()),
                YdotoolOp::Key(key_codes(&["40:1", "40:0"])),
                YdotoolOp::Type("re".to_owned()),
            ]
        );
    }

    #[test]
    fn no_alias_uses_danish_keycodes() {
        assert_eq!(
            build_ydotool_ops("æøå", "no"),
            vec![
                YdotoolOp::Key(key_codes(&["39:1", "39:0"])),
                YdotoolOp::Key(key_codes(&["40:1", "40:0"])),
                YdotoolOp::Key(key_codes(&["26:1", "26:0"])),
            ]
        );
    }

    #[test]
    fn unknown_layout_keeps_unicode_in_type_chunk_for_fallback_behavior() {
        assert_eq!(
            build_ydotool_ops("høre", "us"),
            vec![YdotoolOp::Type("høre".to_owned())]
        );
    }

    #[test]
    fn terminal_or_unknown_target_uses_ctrl_shift_v() {
        assert!(target_prefers_terminal_paste("", ""));
        assert!(target_prefers_terminal_paste(
            "whisper-dictate - Terminal",
            ""
        ));
        assert!(paste_shortcut_args("", "").ends_with(WAYLAND_CTRL_SHIFT_V));
    }

    #[test]
    fn known_text_editor_target_uses_ctrl_v() {
        assert!(!target_prefers_terminal_paste(
            "Text Editor",
            "gnome-text-editor"
        ));
        assert!(paste_shortcut_args("Text Editor", "gnome-text-editor").ends_with(WAYLAND_CTRL_V));
    }

    // -- P2 #391 follow-up: explicit shortcut wins over terminal heuristic --

    use super::super::paste::PasteShortcut;

    #[test]
    fn explicit_ctrl_v_wins_even_on_terminal_target() {
        // The HEADLINE contract: an explicit `Some(CtrlV)` from the caller
        // must NOT be downgraded to Ctrl+Shift+V just because the target
        // looks like a terminal — `paste_shortcut_args_for` honours the
        // explicit choice. The release prelude is still prepended so a
        // still-held PTT modifier can't corrupt the chord.
        let args = paste_shortcut_args_for(Some(PasteShortcut::CtrlV), "Konsole", "konsole");
        assert!(args.starts_with(WAYLAND_MODIFIER_RELEASES));
        assert!(args.ends_with(WAYLAND_CTRL_V));
        assert!(
            !args
                .windows(WAYLAND_CTRL_SHIFT_V.len())
                .any(|w| w == WAYLAND_CTRL_SHIFT_V),
            "explicit CtrlV must not embed the Ctrl+Shift+V chord"
        );
    }

    #[test]
    fn explicit_ctrl_shift_v_wins_even_on_text_editor_target() {
        let args = paste_shortcut_args_for(
            Some(PasteShortcut::CtrlShiftV),
            "Text Editor",
            "gnome-text-editor",
        );
        assert!(args.starts_with(WAYLAND_MODIFIER_RELEASES));
        assert!(args.ends_with(WAYLAND_CTRL_SHIFT_V));
    }

    #[test]
    fn explicit_shift_insert_emits_dedicated_chord() {
        // ShiftInsert previously had no Wayland evdev mapping — the
        // ydotool path effectively didn't support it at all because the
        // helper-only `invoke_paste` was bypassed. New `WAYLAND_SHIFT_INSERT`
        // closes that gap.
        let args = paste_shortcut_args_for(Some(PasteShortcut::ShiftInsert), "anything", "any.exe");
        assert!(args.starts_with(WAYLAND_MODIFIER_RELEASES));
        assert!(args.ends_with(WAYLAND_SHIFT_INSERT));
        // And it must NOT include any of the V-key (KEY_V=47) chords.
        assert!(!args.iter().any(|s| *s == "47:1" || *s == "47:0"));
    }

    #[test]
    fn explicit_cmd_v_emits_dedicated_chord() {
        let args = paste_shortcut_args_for(Some(PasteShortcut::CmdV), "anything", "any.exe");
        assert!(args.starts_with(WAYLAND_MODIFIER_RELEASES));
        assert!(args.ends_with(WAYLAND_CMD_V));
        // Includes KEY_LEFTMETA=125 down then up.
        assert!(args.contains(&"125:1"));
        assert!(args.contains(&"125:0"));
    }

    #[test]
    fn none_falls_back_to_terminal_heuristic_on_terminal_target() {
        // `None` = no explicit preference, so the historical
        // terminal-target heuristic still applies. Mirrors the legacy
        // `paste_shortcut_args` behaviour for back-compat.
        let args = paste_shortcut_args_for(None, "Konsole", "konsole");
        assert!(args.ends_with(WAYLAND_CTRL_SHIFT_V));
    }

    #[test]
    fn none_falls_back_to_ctrl_v_on_non_terminal_target() {
        let args = paste_shortcut_args_for(None, "Text Editor", "gnome-text-editor");
        assert!(args.ends_with(WAYLAND_CTRL_V));
    }

    #[test]
    fn none_with_blank_target_defaults_to_ctrl_shift_v_like_legacy() {
        // The legacy `paste_shortcut_args` treats an empty target as
        // "probably a terminal" (Wayland obscures window identity);
        // `paste_shortcut_args_for(None, ...)` must keep that exact
        // behaviour for back-compat with users who never set a shortcut.
        let args = paste_shortcut_args_for(None, "", "");
        let legacy = paste_shortcut_args("", "");
        assert_eq!(args, legacy);
    }
}
