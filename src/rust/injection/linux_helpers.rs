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
        // dotool's stdin protocol is line-oriented: one command per line,
        // `type <text>` and `key <name>` are the relevant verbs. A literal
        // newline embedded in `<text>` would terminate the `type` command
        // and the following line would be reinterpreted as another
        // command — at best garbling the output, at worst executing a
        // crafted command. P3 #371 finding 3: split the input on '\n'
        // and emit `type <line>` for each segment with `key enter` between
        // them, so a transcript like "line one\nline two" types two lines
        // exactly the way the user wrote them.
        let mut child = Command::new("dotool").stdin(Stdio::piped()).spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            write_dotool_multiline(&mut stdin, text)?;
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

/// Write `text` to a dotool stdin pipe as a sequence of `type <segment>`
/// and `key enter` commands, splitting on `\n` so embedded newlines are
/// safely converted into Enter keypresses rather than terminating the
/// `type` command mid-text.
///
/// Empty leading / trailing / consecutive newlines produce a `key enter`
/// without a `type` line in between, matching the user's intent of "press
/// Enter here". A trailing newline at the very end of `text` also produces
/// a final `key enter` so the cursor ends up on the next line.
///
/// P3 #371 finding 3: pure helper so the line-splitting + escape semantics
/// can be unit-tested without spawning dotool.
fn write_dotool_multiline<W: Write>(stdin: &mut W, text: &str) -> std::io::Result<()> {
    let mut first = true;
    for segment in text.split('\n') {
        if !first {
            writeln!(stdin, "key enter")?;
        }
        if !segment.is_empty() {
            writeln!(stdin, "type {segment}")?;
        }
        first = false;
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

    // -- P3 #371 finding 3: multiline dotool escaping --------------------

    fn dotool_script(text: &str) -> String {
        let mut buf = Vec::new();
        write_dotool_multiline(&mut buf, text).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn dotool_single_line_emits_one_type_command() {
        // Baseline: a line with no embedded newline produces a single
        // `type <text>` followed by a single trailing newline (which dotool
        // requires as its command terminator). No spurious `key enter`.
        assert_eq!(dotool_script("hello world"), "type hello world\n");
    }

    #[test]
    fn dotool_multiline_splits_on_newlines_and_inserts_key_enter() {
        // The headline contract: `line one\nline two` becomes two `type`
        // commands separated by a `key enter`, so dotool types two lines
        // exactly as the user wrote them — instead of treating "line two"
        // as another dotool command after a stray newline terminated the
        // `type` command mid-text.
        assert_eq!(
            dotool_script("line one\nline two"),
            "type line one\nkey enter\ntype line two\n"
        );
    }

    #[test]
    fn dotool_three_lines_chains_two_key_enters() {
        assert_eq!(
            dotool_script("a\nb\nc"),
            "type a\nkey enter\ntype b\nkey enter\ntype c\n"
        );
    }

    #[test]
    fn dotool_empty_line_between_text_emits_two_key_enters() {
        // `foo\n\nbar` = the user pressed Enter twice between paragraphs.
        // We emit `type foo`, two `key enter`s (one for each `\n`), and
        // `type bar` — the empty middle segment skips its `type` but the
        // separator still fires, matching the user's intent.
        assert_eq!(
            dotool_script("foo\n\nbar"),
            "type foo\nkey enter\nkey enter\ntype bar\n"
        );
    }

    #[test]
    fn dotool_trailing_newline_ends_with_key_enter() {
        assert_eq!(dotool_script("hi\n"), "type hi\nkey enter\n");
    }

    #[test]
    fn dotool_leading_newline_starts_with_key_enter() {
        assert_eq!(dotool_script("\nhi"), "key enter\ntype hi\n");
    }

    #[test]
    fn dotool_only_newlines_produces_only_key_enters() {
        assert_eq!(dotool_script("\n\n"), "key enter\nkey enter\n");
    }

    #[test]
    fn dotool_empty_string_produces_no_output() {
        // Defence in depth: an empty input must not write *anything* — not
        // a stray `type ` command (which dotool would reject), not a bare
        // newline. Splitting an empty string on '\n' yields one empty
        // segment which our `if !segment.is_empty()` guard skips.
        assert_eq!(dotool_script(""), "");
    }

    #[test]
    fn dotool_handles_unicode_intact() {
        // Danish characters and other non-ASCII pass through verbatim —
        // dotool itself is byte-transparent, our writer only cares about
        // '\n' splits.
        assert_eq!(
            dotool_script("ærøst\nfløde"),
            "type ærøst\nkey enter\ntype fløde\n"
        );
    }

    #[test]
    fn dotool_carriage_return_alone_is_passed_through() {
        // Documented limitation: only LF triggers the split, not CR. A
        // lone '\r' is part of the `type` payload. The Python wrapper
        // already normalises line endings before reaching us, so a
        // stray CR in the dispatched text is a wrapper bug worth
        // surfacing rather than silently swallowing.
        assert_eq!(dotool_script("a\rb"), "type a\rb\n");
    }
}
