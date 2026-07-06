//! PulseAudio / PipeWire output-mute backend, driven through `pactl`.
//!
//! We deliberately shell out to `pactl` (present on both PulseAudio and
//! PipeWire's `pipewire-pulse` compat shim) rather than link against
//! libpulse: it keeps this feature dep-free on stock Linux builds,
//! matches how PipeWire-only distros expose the sink API, and gives
//! us a trivial in-memory recorder for the integration test. Codex P2
//! (linux.rs:1, PR #440) moved `MockPactl` into `linux_tests.rs`
//! alongside the tests it serves.
//!
//! # Commands
//!
//! * `pactl get-sink-mute @DEFAULT_SINK@` prints `Mute: yes` or
//!   `Mute: no` on stdout (case-insensitive). We use `@DEFAULT_SINK@`
//!   rather than a numeric index so PipeWire and PulseAudio both
//!   route to the current default without us having to enumerate.
//! * `pactl set-sink-mute @DEFAULT_SINK@ <1|0>` mutes or unmutes.
//!
//! The command runner is behind the [`PactlRunner`] trait so unit +
//! integration tests can substitute a recorder without spawning a
//! subprocess.

use std::process::Command;

use crate::output_mute::{MuteError, OutputMuteBackend};

/// The `@DEFAULT_SINK@` symbolic name that both PulseAudio and PipeWire
/// resolve to the current default render endpoint. Prefer this to a
/// numeric sink index so a device-switch mid-session still works.
pub const DEFAULT_SINK: &str = "@DEFAULT_SINK@";

/// Outcome of one `pactl` invocation, split so the backend does not
/// need to care whether the exit status came from a real subprocess or
/// a test recorder.
#[derive(Debug, Clone)]
pub struct PactlResult {
    pub status_ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The subprocess boundary the backend calls into. Real code drives
/// `std::process::Command`; tests substitute a recorder that captures
/// the arguments without executing anything.
pub trait PactlRunner: Send + Sync {
    fn run(&self, args: &[&str]) -> Result<PactlResult, MuteError>;
}

/// Real-subprocess implementation of [`PactlRunner`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPactl;

impl PactlRunner for SystemPactl {
    fn run(&self, args: &[&str]) -> Result<PactlResult, MuteError> {
        let output = Command::new("pactl")
            .args(args)
            .output()
            .map_err(|err| MuteError::Unavailable(format!("pactl spawn failed: {err}")))?;
        Ok(PactlResult {
            status_ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Output-mute backend that talks to PulseAudio / PipeWire via `pactl`.
///
/// Generic in the runner so a test recorder (`MockPactl` in
/// `linux_tests.rs`) can be swapped in for tests.
/// The default constructor uses the real subprocess runner.
///
/// Codex P2 (state.rs:175, PR #440) — [`Self::pin_current_endpoint`]
/// resolves `@DEFAULT_SINK@` to a specific sink name via
/// `pactl get-default-sink` and caches it in [`Self::pinned_sink`]. All
/// subsequent `get_mute` / `set_mute` calls target that concrete sink
/// until the pin is cleared, so a mid-recording default-device switch
/// does not leave the original speakers muted / silently unmute a
/// newly-selected device.
pub struct PactlBackend<R: PactlRunner = SystemPactl> {
    runner: R,
    /// Pinned concrete sink name (e.g. `alsa_output.pci-0000_00_1f.3.analog-stereo`).
    /// `None` → fall back to the `@DEFAULT_SINK@` symbolic name.
    pinned_sink: std::sync::Mutex<Option<String>>,
}

impl Default for PactlBackend<SystemPactl> {
    fn default() -> Self {
        Self {
            runner: SystemPactl,
            pinned_sink: std::sync::Mutex::new(None),
        }
    }
}

impl<R: PactlRunner> PactlBackend<R> {
    /// Construct a backend around an arbitrary runner. Used by tests
    /// and by callers that want a custom `pactl` path.
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            pinned_sink: std::sync::Mutex::new(None),
        }
    }

    /// Resolve the sink identifier that get/set calls should target:
    /// the pinned concrete name if one is set, otherwise the
    /// `@DEFAULT_SINK@` symbolic name.
    fn effective_sink(&self) -> String {
        self.pinned_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| DEFAULT_SINK.to_owned())
    }
}

impl<R: PactlRunner> OutputMuteBackend for PactlBackend<R> {
    fn get_mute(&self) -> Result<bool, MuteError> {
        let sink = self.effective_sink();
        let result = self.runner.run(&["get-sink-mute", &sink])?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "pactl get-sink-mute failed: {}",
                result.stderr.trim(),
            )));
        }
        parse_get_sink_mute(&result.stdout)
    }

    fn set_mute(&self, muted: bool) -> Result<(), MuteError> {
        let flag = if muted { "1" } else { "0" };
        let sink = self.effective_sink();
        let result = self.runner.run(&["set-sink-mute", &sink, flag])?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "pactl set-sink-mute {flag} failed: {}",
                result.stderr.trim(),
            )));
        }
        Ok(())
    }

    fn pin_current_endpoint(&self) -> Result<(), MuteError> {
        // Codex P2 (state.rs:175, PR #440) — resolve @DEFAULT_SINK@ to
        // a concrete sink name once at recording start so a mid-session
        // default-device switch does not misroute the restore.
        let result = self.runner.run(&["get-default-sink"])?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "pactl get-default-sink failed: {}",
                result.stderr.trim(),
            )));
        }
        let name = result.stdout.trim();
        if name.is_empty() {
            return Err(MuteError::UnexpectedOutput(format!(
                "pactl get-default-sink produced no sink name: {:?}",
                result.stdout,
            )));
        }
        *self.pinned_sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.to_owned());
        Ok(())
    }

    fn clear_endpoint_pin(&self) {
        *self.pinned_sink.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Parse a `pactl get-sink-mute @DEFAULT_SINK@` line.
///
/// `pactl` prints `Mute: yes` / `Mute: no` (English regardless of
/// locale — pactl explicitly hardcodes those tokens). We accept any
/// case + surrounding whitespace so a version-drift in spacing does
/// not silently break the parse.
pub fn parse_get_sink_mute(stdout: &str) -> Result<bool, MuteError> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Mute:")
            .or_else(|| trimmed.strip_prefix("mute:"))
            .or_else(|| trimmed.strip_prefix("MUTE:"))
        {
            match rest.trim().to_ascii_lowercase().as_str() {
                "yes" | "1" | "true" => return Ok(true),
                "no" | "0" | "false" => return Ok(false),
                _ => {
                    return Err(MuteError::UnexpectedOutput(format!(
                        "unrecognised pactl mute value: {rest:?}",
                    )))
                }
            }
        }
    }
    Err(MuteError::UnexpectedOutput(format!(
        "no `Mute:` line in pactl output: {stdout:?}",
    )))
}


// Codex P2 (linux.rs:1, PR #440) — MockPactl + tests live in a sibling
// file (`linux_tests.rs`) so this module stays under AGENTS.md's
// ~500-LOC modularity cap. Impl + mock + tests inline previously
// weighed 523 lines.
#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
