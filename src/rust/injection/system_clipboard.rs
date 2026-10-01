//! Small subprocess-backed system clipboard used by native dictation.
//!
//! Keeping this behind the existing [`Clipboard`] trait avoids a new native
//! dependency and matches the clipboard tools already supported by the
//! history command.

use std::process::{Command, Stdio};
use std::time::Duration;

const CLIPBOARD_READ_LIMIT: usize = 1024 * 1024;

use super::paste::Clipboard;

pub struct SystemClipboard {
    selected: Option<Candidate>,
    candidates: Vec<Candidate>,
    runner: Box<dyn CommandRunner>,
    readable: bool,
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
            candidates: candidates(),
            runner: Box::new(NativeCommandRunner),
            readable: false,
        }
    }
}

impl Clipboard for SystemClipboard {
    fn read(&mut self) -> Option<String> {
        self.readable = false;
        for candidate in self.candidates.iter().copied() {
            if let Some(value) = self.runner.read(candidate) {
                self.selected = Some(candidate);
                self.readable = true;
                return Some(value);
            }
        }
        None
    }

    fn write(&mut self, value: &str) -> bool {
        // Paste injection always reads before writing. If no helper could
        // return a valid UTF-8 selection, overwriting would destroy an
        // unreadable image/binary clipboard with no restorable backup.
        if !self.readable {
            return false;
        }
        // The backup returned by read() belongs to this exact clipboard
        // backend. Falling through to another writer would overwrite a
        // different selection without having saved its prior contents.
        self.selected
            .is_some_and(|candidate| self.runner.write(candidate, value))
    }
}

#[cfg(test)]
impl SystemClipboard {
    pub(super) fn with_runner(candidates: Vec<Candidate>, runner: Box<dyn CommandRunner>) -> Self {
        Self {
            selected: None,
            candidates,
            runner,
            readable: false,
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

pub(super) fn run_read(candidate: Candidate) -> Option<String> {
    let mut command = Command::new(candidate.program);
    command
        .args(candidate.read_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
    let output = crate::bounded_process::run(
        &mut command,
        None,
        Duration::from_secs(2),
        CLIPBOARD_READ_LIMIT,
        &|| true,
    )
    .ok()?;
    if output.completion != crate::bounded_process::Completion::Exited
        || !output.status.success()
        || output.stdout_truncated
    {
        return None;
    }
    decode_text(output.stdout)
}

pub(super) fn run_write(candidate: Candidate, value: &str) -> bool {
    let mut command = Command::new(write_program(candidate));
    command
        .args(candidate.write_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
    crate::bounded_process::run_clipboard_owner(
        &mut command,
        value.as_bytes().to_vec(),
        crate::bounded_process::HELPER_TIMEOUT,
    )
    .is_ok_and(|output| {
        output.completion == crate::bounded_process::Completion::Exited
            && output.status.success()
            && output.stdin_error.is_none()
    })
}

pub(super) fn decode_text(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes).ok()
}

pub(super) fn write_program(candidate: Candidate) -> &'static str {
    if candidate.program == "wl-paste" {
        "wl-copy"
    } else {
        candidate.program
    }
}
