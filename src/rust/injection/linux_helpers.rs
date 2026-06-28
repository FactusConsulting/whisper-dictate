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

/// xdotool key-up arguments that drain every common modifier (both sides)
/// in one invocation. Mirrors the evdev `WAYLAND_MODIFIER_RELEASES` list
/// but uses xdotool's symbolic key names — `keyup` sends a bare key-up
/// without a preceding press, which is exactly what we need to clear a
/// PTT chord held through the dictation. Pulled out as a constant so the
/// unit test can pin the exact argument vector.
///
/// Codex P2 #419 dispatcher.rs:184 — when the helper-chain inject path
/// runs without `--clearmodifiers` (kwtype / wtype / dotool), this
/// fallback releases stale Ctrl / Shift / Alt / Super on both sides via
/// xdotool before the inject lands.
pub const XDOTOOL_MODIFIER_KEYUP_ARGS: &[&str] = &[
    "keyup",
    "ctrl",
    "shift",
    "alt",
    "super",
    "Control_L",
    "Control_R",
    "Shift_L",
    "Shift_R",
    "Alt_L",
    "Alt_R",
    "Super_L",
    "Super_R",
];

/// Best-effort modifier release for the Linux helper-chain inject path.
///
/// Called from `Injector::release_held_modifiers` when no trait-object
/// backend is installed. Picks the most-reliable available release
/// mechanism *regardless* of which helper will run the actual inject —
/// because `wtype`/`kwtype`/`dotool` have no first-class "release stale
/// modifier" verb, but the user usually has `xdotool` or `ydotool`
/// installed alongside.
///
/// Order:
/// 1. **ydotool** — Wayland-native, sends evdev key-ups via the daemon
///    socket; works in both X11 and Wayland sessions.
/// 2. **xdotool** — X11 fallback (`keyup` verb sends a bare key-up).
/// 3. Neither installed → silent `Ok` (best effort, matching the rest of
///    the stale-modifier path's permissive philosophy: losing a release
///    is strictly less bad than failing the inject).
///
/// `locator` is injected so unit tests can simulate "only xdotool
/// installed" / "neither installed" without touching `$PATH`. Codex P2
/// #419 dispatcher.rs:184.
pub fn release_modifiers_best_effort<F>(locator: F) -> Result<()>
where
    F: Fn(&str) -> Option<std::path::PathBuf>,
{
    let plan = plan_modifier_release(&locator);
    let Some((helper, args)) = plan else {
        // Neither ydotool nor xdotool present — nothing to do. A
        // wtype/kwtype/dotool-only host with a PTT modifier still down
        // is rare in practice (those tools are Wayland-specific and
        // Wayland sessions almost always have ydotool too).
        return Ok(());
    };
    let output = Command::new(helper).args(&args).output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "{helper} modifier release failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Pure decision: which helper to use for the release sweep and the exact
/// argument vector to pass. Split from [`release_modifiers_best_effort`]
/// so the choice is unit-testable without spawning a child process.
pub fn plan_modifier_release<F>(locator: &F) -> Option<(&'static str, Vec<String>)>
where
    F: Fn(&str) -> Option<std::path::PathBuf>,
{
    if locator("ydotool").is_some() {
        let mut args = vec!["key".to_owned()];
        args.extend(
            super::wayland::WAYLAND_MODIFIER_RELEASES
                .iter()
                .map(|s| (*s).to_owned()),
        );
        Some(("ydotool", args))
    } else if locator("xdotool").is_some() {
        Some((
            "xdotool",
            XDOTOOL_MODIFIER_KEYUP_ARGS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        ))
    } else {
        None
    }
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

    // -- Codex P2 #419 dispatcher.rs:184: helper-chain release sweep -----

    use std::path::PathBuf;

    fn locator_for(present: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| {
            if present.contains(&name) {
                Some(PathBuf::from(format!("/usr/bin/{name}")))
            } else {
                None
            }
        }
    }

    #[test]
    fn plan_picks_ydotool_when_available_and_uses_full_wayland_release_list() {
        // ydotool is the first choice: it works on both Wayland and X11
        // and reaches the OS via the uinput daemon, so it can release
        // modifiers regardless of which inject-helper wins the chain.
        // The arg vector must mirror `WAYLAND_MODIFIER_RELEASES` verbatim
        // (drained as evdev key-up codes) so a regression that drops a
        // side-specific scancode is caught.
        let plan = plan_modifier_release(&locator_for(&["ydotool", "xdotool", "wtype"]));
        let (helper, args) = plan.expect("ydotool should be chosen when present");
        assert_eq!(helper, "ydotool");
        assert_eq!(args[0], "key");
        let codes: Vec<&str> = args[1..].iter().map(String::as_str).collect();
        let expected: Vec<&str> = super::super::wayland::WAYLAND_MODIFIER_RELEASES.to_vec();
        assert_eq!(codes, expected);
    }

    #[test]
    fn plan_falls_back_to_xdotool_when_only_xdotool_is_installed() {
        // X11-only host without ydotool: xdotool's `keyup` verb sends a
        // bare key-up without a preceding press, which is exactly what
        // we need to clear a held PTT modifier. The argument vector is
        // pinned so a future tweak that drops a side-specific name
        // (e.g. Control_R) fails the test instead of silently shipping.
        let plan = plan_modifier_release(&locator_for(&["xdotool", "wtype", "kwtype"]));
        let (helper, args) = plan.expect("xdotool should win when ydotool is absent");
        assert_eq!(helper, "xdotool");
        let strs: Vec<&str> = args.iter().map(String::as_str).collect();
        assert_eq!(strs, XDOTOOL_MODIFIER_KEYUP_ARGS.to_vec());
        // And it must include both sides of every common modifier.
        for name in [
            "Control_L",
            "Control_R",
            "Shift_L",
            "Shift_R",
            "Alt_L",
            "Alt_R",
            "Super_L",
            "Super_R",
        ] {
            assert!(
                strs.contains(&name),
                "xdotool release must cover {name}, got {strs:?}"
            );
        }
    }

    #[test]
    fn plan_is_none_when_no_release_helper_is_installed() {
        // wtype/kwtype/dotool can't perform a stale-modifier release on
        // their own (no `keyup` / `--clearmodifiers` equivalent). On a
        // host that has *only* those installed we return `None` so the
        // dispatcher resolves to a silent `Ok`, matching the existing
        // permissive philosophy ("losing a release is less bad than
        // failing the inject").
        assert!(plan_modifier_release(&locator_for(&["wtype", "kwtype", "dotool"])).is_none());
        assert!(plan_modifier_release(&locator_for(&[])).is_none());
    }

    #[test]
    fn plan_prefers_ydotool_even_when_xdotool_also_present() {
        // Stability guard for the priority order: a host with BOTH tools
        // installed (common: ydotool for Wayland, xdotool kept for X11
        // tooling) must always pick ydotool because it works in both
        // sessions and avoids the X11-only assumption.
        let plan = plan_modifier_release(&locator_for(&["xdotool", "ydotool"]));
        assert_eq!(plan.expect("expected a plan").0, "ydotool");
    }
}
