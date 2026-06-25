//! Per-helper command-line constructors for the Linux fallback chain.
//!
//! Each helper (`kwtype`, `wtype`, `dotool`, `xdotool`) has its own
//! command syntax. Centralising those here keeps `dispatcher.rs` short and
//! lets the chord-builder logic be unit-tested without touching `Command`.
//! Only compiled in on Linux; on other platforms enigo handles everything.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};

use super::paste::PasteShortcut;

/// Build the `type <text>` invocation for a non-ydotool helper and run it.
/// `ydotool` itself takes the keymap-aware path in `super::wayland`.
pub fn invoke_type(helper: &str, text: &str) -> Result<()> {
    if helper == "dotool" {
        // dotool reads commands from stdin (`type <text>` per line).
        let mut child = Command::new("dotool").stdin(Stdio::piped()).spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            writeln!(stdin, "type {text}")?;
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(anyhow!("dotool exited with {status}"));
        }
        return Ok(());
    }
    let mut cmd = Command::new(helper);
    match helper {
        "wtype" | "kwtype" => {
            cmd.arg("--").arg(text);
        }
        "xdotool" => {
            cmd.args(["type", "--clearmodifiers", "--"]).arg(text);
        }
        other => return Err(anyhow!("unknown helper: {other}")),
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "{helper} type failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Send the requested paste shortcut via `helper`.
pub fn invoke_paste(helper: &str, shortcut: PasteShortcut) -> Result<()> {
    let chord = shortcut_to_helper_chord(helper, shortcut)?;
    let output = Command::new(helper).args(chord).output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "{helper} paste failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Helper-specific paste-chord arguments. Mirrors the documented syntaxes
/// (`man xdotool`, `wtype --help`). Pure function so it can be unit-tested.
pub fn shortcut_to_helper_chord(helper: &str, shortcut: PasteShortcut) -> Result<Vec<String>> {
    Ok(match (helper, shortcut) {
        ("xdotool", PasteShortcut::CtrlV) => {
            vec!["key".into(), "--clearmodifiers".into(), "ctrl+v".into()]
        }
        ("xdotool", PasteShortcut::CtrlShiftV) => {
            vec![
                "key".into(),
                "--clearmodifiers".into(),
                "ctrl+shift+v".into(),
            ]
        }
        ("xdotool", PasteShortcut::ShiftInsert) => {
            vec![
                "key".into(),
                "--clearmodifiers".into(),
                "shift+Insert".into(),
            ]
        }
        ("xdotool", PasteShortcut::CmdV) => {
            vec!["key".into(), "--clearmodifiers".into(), "super+v".into()]
        }
        ("wtype" | "kwtype", PasteShortcut::CtrlV) => vec![
            "-M".into(),
            "ctrl".into(),
            "v".into(),
            "-m".into(),
            "ctrl".into(),
        ],
        ("wtype" | "kwtype", PasteShortcut::CtrlShiftV) => vec![
            "-M".into(),
            "ctrl".into(),
            "-M".into(),
            "shift".into(),
            "v".into(),
            "-m".into(),
            "shift".into(),
            "-m".into(),
            "ctrl".into(),
        ],
        ("wtype" | "kwtype", PasteShortcut::ShiftInsert) => vec![
            "-M".into(),
            "shift".into(),
            "-k".into(),
            "Insert".into(),
            "-m".into(),
            "shift".into(),
        ],
        ("wtype" | "kwtype", PasteShortcut::CmdV) => vec![
            "-M".into(),
            "logo".into(),
            "v".into(),
            "-m".into(),
            "logo".into(),
        ],
        ("dotool", shortcut) => {
            return Err(anyhow!(
                "dotool paste shortcut {:?} not implemented; install ydotool",
                shortcut
            ));
        }
        (helper, _) => return Err(anyhow!("unknown helper: {helper}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdotool_ctrl_v_uses_documented_chord() {
        assert_eq!(
            shortcut_to_helper_chord("xdotool", PasteShortcut::CtrlV).unwrap(),
            vec!["key", "--clearmodifiers", "ctrl+v"]
        );
    }

    #[test]
    fn xdotool_ctrl_shift_v_uses_documented_chord() {
        assert_eq!(
            shortcut_to_helper_chord("xdotool", PasteShortcut::CtrlShiftV).unwrap(),
            vec!["key", "--clearmodifiers", "ctrl+shift+v"]
        );
    }

    #[test]
    fn xdotool_shift_insert_uses_documented_chord() {
        assert_eq!(
            shortcut_to_helper_chord("xdotool", PasteShortcut::ShiftInsert).unwrap(),
            vec!["key", "--clearmodifiers", "shift+Insert"]
        );
    }

    #[test]
    fn wtype_ctrl_v_uses_modifier_flags() {
        assert_eq!(
            shortcut_to_helper_chord("wtype", PasteShortcut::CtrlV).unwrap(),
            vec!["-M", "ctrl", "v", "-m", "ctrl"]
        );
    }

    #[test]
    fn kwtype_uses_same_syntax_as_wtype() {
        assert_eq!(
            shortcut_to_helper_chord("kwtype", PasteShortcut::CtrlShiftV).unwrap(),
            shortcut_to_helper_chord("wtype", PasteShortcut::CtrlShiftV).unwrap()
        );
    }

    #[test]
    fn dotool_paste_is_not_implemented() {
        assert!(shortcut_to_helper_chord("dotool", PasteShortcut::CtrlV).is_err());
    }

    #[test]
    fn unknown_helper_errors() {
        assert!(shortcut_to_helper_chord("xte", PasteShortcut::CtrlV).is_err());
    }
}
