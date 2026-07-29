//! Small subprocess-backed system clipboard used by native dictation.
//!
//! Keeping this behind the existing [`Clipboard`] trait avoids a new native
//! dependency and matches the clipboard tools already supported by the
//! history command.

use std::io::Write;
use std::process::{Command, Stdio};

use super::paste::Clipboard;

pub struct SystemClipboard {
    selected: Option<Candidate>,
    runner: Box<dyn CommandRunner>,
}

impl std::fmt::Debug for SystemClipboard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SystemClipboard")
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self {
            selected: None,
            runner: Box::new(NativeCommandRunner),
        }
    }
}

impl Clipboard for SystemClipboard {
    fn read(&mut self) -> Option<String> {
        for candidate in candidates() {
            if let Some(value) = self.runner.read(candidate) {
                self.selected = Some(candidate);
                return Some(value);
            }
        }
        None
    }

    fn write(&mut self, value: &str) -> bool {
        if self
            .selected
            .is_some_and(|candidate| self.runner.write(candidate, value))
        {
            return true;
        }
        for candidate in candidates() {
            if Some(candidate) != self.selected && self.runner.write(candidate, value) {
                self.selected = Some(candidate);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
impl SystemClipboard {
    pub(super) fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        Self {
            selected: None,
            runner,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) program: &'static str,
    pub(super) read_args: &'static [&'static str],
    pub(super) write_args: &'static [&'static str],
}

#[cfg(target_os = "linux")]
fn candidates() -> Vec<Candidate> {
    let wayland = std::env::var("WAYLAND_DISPLAY").is_ok_and(|value| !value.trim().is_empty())
        || std::env::var("XDG_SESSION_TYPE")
            .is_ok_and(|value| value.trim().eq_ignore_ascii_case("wayland"));
    linux_candidates(wayland)
}

#[cfg(target_os = "linux")]
pub(super) fn linux_candidates(wayland: bool) -> Vec<Candidate> {
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

pub(super) trait CommandRunner: Send {
    fn read(&mut self, candidate: Candidate) -> Option<String>;
    fn write(&mut self, candidate: Candidate, value: &str) -> bool;
}

struct NativeCommandRunner;

impl CommandRunner for NativeCommandRunner {
    fn read(&mut self, candidate: Candidate) -> Option<String> {
        run_read(candidate)
    }

    fn write(&mut self, candidate: Candidate, value: &str) -> bool {
        run_write(candidate, value)
    }
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
    let Ok(mut child) = Command::new(write_program(candidate))
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

pub(super) fn write_program(candidate: Candidate) -> &'static str {
    if candidate.program == "wl-paste" {
        "wl-copy"
    } else {
        candidate.program
    }
}
