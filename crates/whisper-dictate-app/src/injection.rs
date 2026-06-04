use std::process::Command;

use anyhow::{anyhow, Result};

const WAYLAND_MODIFIER_RELEASES: &[&str] = &[
    "29:0",  // KEY_LEFTCTRL
    "97:0",  // KEY_RIGHTCTRL
    "42:0",  // KEY_LEFTSHIFT
    "54:0",  // KEY_RIGHTSHIFT
    "56:0",  // KEY_LEFTALT
    "100:0", // KEY_RIGHTALT
    "125:0", // KEY_LEFTMETA
    "126:0", // KEY_RIGHTMETA
];
const WAYLAND_CTRL_V: &[&str] = &["29:1", "47:1", "47:0", "29:0"];
const WAYLAND_CTRL_SHIFT_V: &[&str] = &["29:1", "42:1", "47:1", "47:0", "42:0", "29:0"];
const LINUX_TERMINAL_TARGETS: &[&str] = &[
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
    Key(Vec<&'static str>),
}

pub fn handle_inject_text(
    mode: &str,
    text: &str,
    xkb_layout: &str,
    target_title: &str,
    target_process: &str,
) -> Result<()> {
    match mode {
        "type" => type_text(text, xkb_layout),
        "paste" => paste_shortcut(target_title, target_process),
        other => Err(anyhow!("unsupported inject-text mode: {other}")),
    }
}

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

pub fn target_prefers_terminal_paste(target_title: &str, target_process: &str) -> bool {
    let target = format!("{target_title} {target_process}").to_lowercase();
    if target.trim().is_empty() {
        return true;
    }
    LINUX_TERMINAL_TARGETS
        .iter()
        .any(|term| target.contains(term))
}

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

fn type_text(text: &str, xkb_layout: &str) -> Result<()> {
    for op in build_ydotool_ops(text, xkb_layout) {
        match op {
            YdotoolOp::Type(chunk) => run_ydotool(["type", "--", &chunk])?,
            YdotoolOp::Key(keys) => {
                let mut args = vec!["key"];
                args.extend(keys);
                run_ydotool(args)?;
            }
        }
    }
    Ok(())
}

fn paste_shortcut(target_title: &str, target_process: &str) -> Result<()> {
    run_ydotool(std::iter::once("key").chain(paste_shortcut_args(target_title, target_process)))
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

fn keycodes_for(layout: &str, ch: char) -> Option<Vec<&'static str>> {
    let layout = if layout == "no" { "dk" } else { layout };
    match layout {
        "dk" => dk_keycodes(ch),
        "se" => se_keycodes(ch),
        "de" => de_keycodes(ch),
        "fi" => fi_keycodes(ch),
        _ => None,
    }
}

fn dk_keycodes(ch: char) -> Option<Vec<&'static str>> {
    match ch {
        'å' => Some(vec!["26:1", "26:0"]),
        'Å' => Some(vec!["42:1", "26:1", "26:0", "42:0"]),
        'æ' => Some(vec!["39:1", "39:0"]),
        'Æ' => Some(vec!["42:1", "39:1", "39:0", "42:0"]),
        'ø' => Some(vec!["40:1", "40:0"]),
        'Ø' => Some(vec!["42:1", "40:1", "40:0", "42:0"]),
        _ => nordic_de_punct(ch),
    }
}

fn se_keycodes(ch: char) -> Option<Vec<&'static str>> {
    match ch {
        'å' => Some(vec!["26:1", "26:0"]),
        'Å' => Some(vec!["42:1", "26:1", "26:0", "42:0"]),
        'ä' => Some(vec!["40:1", "40:0"]),
        'Ä' => Some(vec!["42:1", "40:1", "40:0", "42:0"]),
        'ö' => Some(vec!["39:1", "39:0"]),
        'Ö' => Some(vec!["42:1", "39:1", "39:0", "42:0"]),
        _ => nordic_de_punct(ch),
    }
}

fn de_keycodes(ch: char) -> Option<Vec<&'static str>> {
    match ch {
        'ä' => Some(vec!["40:1", "40:0"]),
        'Ä' => Some(vec!["42:1", "40:1", "40:0", "42:0"]),
        'ö' => Some(vec!["39:1", "39:0"]),
        'Ö' => Some(vec!["42:1", "39:1", "39:0", "42:0"]),
        'ü' => Some(vec!["26:1", "26:0"]),
        'Ü' => Some(vec!["42:1", "26:1", "26:0", "42:0"]),
        _ => nordic_de_punct(ch),
    }
}

fn fi_keycodes(ch: char) -> Option<Vec<&'static str>> {
    match ch {
        'ä' => Some(vec!["40:1", "40:0"]),
        'Ä' => Some(vec!["42:1", "40:1", "40:0", "42:0"]),
        'ö' => Some(vec!["39:1", "39:0"]),
        'Ö' => Some(vec!["42:1", "39:1", "39:0", "42:0"]),
        _ => nordic_de_punct(ch),
    }
}

fn nordic_de_punct(ch: char) -> Option<Vec<&'static str>> {
    match ch {
        '?' => Some(vec!["42:1", "12:1", "12:0", "42:0"]),
        '-' => Some(vec!["53:1", "53:0"]),
        '_' => Some(vec!["42:1", "53:1", "53:0", "42:0"]),
        ':' => Some(vec!["42:1", "52:1", "52:0", "42:0"]),
        ';' => Some(vec!["42:1", "51:1", "51:0", "42:0"]),
        '/' => Some(vec!["42:1", "8:1", "8:0", "42:0"]),
        '"' => Some(vec!["42:1", "3:1", "3:0", "42:0"]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dk_direct_text_splits_special_chars() {
        assert_eq!(
            build_ydotool_ops("høre", "dk"),
            vec![
                YdotoolOp::Type("h".to_owned()),
                YdotoolOp::Key(vec!["40:1", "40:0"]),
                YdotoolOp::Type("re".to_owned()),
            ]
        );
    }

    #[test]
    fn no_alias_uses_danish_keycodes() {
        assert_eq!(
            build_ydotool_ops("æøå", "no"),
            vec![
                YdotoolOp::Key(vec!["39:1", "39:0"]),
                YdotoolOp::Key(vec!["40:1", "40:0"]),
                YdotoolOp::Key(vec!["26:1", "26:0"]),
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
}
