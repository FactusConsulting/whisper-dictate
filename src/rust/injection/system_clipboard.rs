//! Small subprocess-backed system clipboard used by native dictation.
//!
//! Keeping this behind the existing [`Clipboard`] trait avoids a new native
//! dependency and matches the clipboard tools already supported by the
//! history command.

use std::io::Write;
use std::process::{Command, Stdio};

use super::paste::Clipboard;

#[derive(Debug, Default)]
pub struct SystemClipboard {
    selected: Option<Candidate>,
}

impl Clipboard for SystemClipboard {
    fn read(&mut self) -> Option<String> {
        for candidate in candidates() {
            if let Some(value) = run_read(candidate) {
                self.selected = Some(candidate);
                return Some(value);
            }
        }
        None
    }

    fn write(&mut self, value: &str) -> bool {
        if self
            .selected
            .is_some_and(|candidate| run_write(candidate, value))
        {
            return true;
        }
        for candidate in candidates() {
            if Some(candidate) != self.selected && run_write(candidate, value) {
                self.selected = Some(candidate);
                return true;
            }
        }
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate {
    program: &'static str,
    read_args: &'static [&'static str],
    write_args: &'static [&'static str],
}

#[cfg(target_os = "linux")]
fn candidates() -> Vec<Candidate> {
    let wayland = std::env::var("WAYLAND_DISPLAY").is_ok_and(|value| !value.trim().is_empty())
        || std::env::var("XDG_SESSION_TYPE")
            .is_ok_and(|value| value.trim().eq_ignore_ascii_case("wayland"));
    linux_candidates(wayland)
}

#[cfg(target_os = "linux")]
fn linux_candidates(wayland: bool) -> Vec<Candidate> {
    let wl = Candidate {
        program: "wl-paste",
        read_args: &["--no-newline"],
        write_args: &[],
    };
    let xclip = Candidate {
        program: "xclip",
        read_args: &["-selection", "clipboard", "-out"],
        write_args: &["-selection", "clipboard", "-in"],
    };
    let xsel = Candidate {
        program: "xsel",
        read_args: &["--clipboard", "--output"],
        write_args: &["--clipboard", "--input"],
    };
    if wayland {
        vec![wl, xclip, xsel]
    } else {
        vec![xclip, xsel, wl]
    }
}

#[cfg(not(target_os = "linux"))]
fn candidates() -> Vec<Candidate> {
    Vec::new()
}

fn run_read(candidate: Candidate) -> Option<String> {
    let output = Command::new(candidate.program)
        .args(candidate.read_args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_write(candidate: Candidate, value: &str) -> bool {
    // wl-copy is the writer paired with wl-paste; the other platforms use
    // one executable for both directions.
    let program = if candidate.program == "wl-paste" {
        "wl-copy"
    } else {
        candidate.program
    };
    let Ok(mut child) = Command::new(program)
        .args(candidate.write_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(value.as_bytes()).is_ok());
    wrote && child.wait().is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn wayland_prefers_native_clipboard_tools() {
        let names: Vec<_> = linux_candidates(true)
            .into_iter()
            .map(|c| c.program)
            .collect();
        assert_eq!(names, ["wl-paste", "xclip", "xsel"]);
    }
}
