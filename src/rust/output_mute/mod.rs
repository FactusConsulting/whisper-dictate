//! Auto-mute the system audio output for the duration of a recording
//! (issue #322).
//!
//! Motivation: when the user starts dictating during a meeting or with music
//! playing, whatever is coming out of the speakers can bleed into the
//! microphone and contaminate the transcription — especially on open-mic /
//! no-echo-cancellation setups. Muting the default render endpoint for the
//! recording window removes that contamination class cleanly and restores
//! the previous mute state (even on panic / abrupt drop) when recording
//! ends.
//!
//! The feature is behind the [`AppSettings::mute_output_while_recording`]
//! toggle (default OFF); nothing runs unless the user opts in.
//!
//! # Layout
//!
//! * [`OutputMuteBackend`] is the small OS-facing trait: `get_mute` +
//!   `set_mute`. Backends live in `linux.rs` (pactl subprocess),
//!   `windows.rs` (PowerShell + CoreAudio interop) and `noop.rs`
//!   (macOS/other + unit-test default).
//! * [`state`] owns the save/restore state machine: it remembers whether
//!   *we* muted the output and, if so, what the user's prior mute state
//!   was so we only restore what we changed.
//! * [`session`] holds a process-global controller that the supervisor's
//!   worker-event stream can observe without threading an `Arc` through
//!   every layer. Absent init → all observation calls are cheap no-ops.
//!
//! The trait boundary keeps the Linux integration test honest: it swaps a
//! [`MockPactl`](linux::MockPactl) recorder for the real subprocess so we
//! can assert the exact `pactl` verbs we emit without touching the host
//! audio state.
//!
//! [`AppSettings::mute_output_while_recording`]: crate::config::AppSettings::mute_output_while_recording

use std::sync::Arc;

pub mod session;
pub mod state;

mod noop;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "windows")]
pub mod windows;

pub use noop::NoopBackend;
pub use state::{MuteController, MuteError, PriorMuteState};

/// A minimal, mockable OS boundary for muting the default render endpoint.
///
/// Implementors report the current mute state (`get_mute`) so the
/// controller can save it, and apply a new mute state (`set_mute`) that
/// the controller drives from the recording lifecycle. Every call is
/// fallible so backends can surface transient errors (missing tool,
/// COM failure, permission denied) without panicking the audio path.
///
/// Codex P2 (state.rs:175, PR #440) — [`Self::pin_current_endpoint`] +
/// [`Self::clear_endpoint_pin`] let backends bind a recording session
/// to a *specific* endpoint rather than re-resolving the default on
/// every call. If the user switches the default output device
/// mid-recording, the restore then targets the endpoint we originally
/// muted, not whatever the OS now considers default (which the user
/// may have deliberately muted before switching).
///
/// Both methods have no-op default impls so backends that either don't
/// need this (macOS/noop) or haven't been ported yet keep the previous
/// "always default" behaviour. Linux implements it via
/// `pactl get-default-sink`.
pub trait OutputMuteBackend: Send + Sync {
    /// Read the current mute state of the default render endpoint.
    fn get_mute(&self) -> Result<bool, MuteError>;

    /// Set the mute state of the default render endpoint.
    fn set_mute(&self, muted: bool) -> Result<(), MuteError>;

    /// Resolve the current default render endpoint and cache it inside
    /// the backend so subsequent [`Self::get_mute`] / [`Self::set_mute`]
    /// calls target that specific endpoint until [`Self::clear_endpoint_pin`]
    /// is called. Default impl is a no-op — backends that don't need
    /// pinning simply keep re-resolving the default on every call.
    ///
    /// Codex P2 (state.rs:175, PR #440) — used by [`MuteController`] at
    /// recording start so a mid-recording default-device switch does
    /// not leave the original speakers muted / silently unmute a
    /// newly-selected device the user had already muted.
    fn pin_current_endpoint(&self) -> Result<(), MuteError> {
        Ok(())
    }

    /// Release the endpoint pin installed by [`Self::pin_current_endpoint`]
    /// so subsequent calls fall back to the current default endpoint.
    /// No-op by default; called by [`MuteController`] on stop / drop.
    fn clear_endpoint_pin(&self) {}
}

/// Build the platform-appropriate backend.
///
/// Linux uses PulseAudio/PipeWire's `pactl` (present on both stacks via
/// PipeWire's compatibility shim). Windows drives CoreAudio through a
/// small PowerShell snippet. macOS is deferred per the issue and falls
/// back to [`NoopBackend`] — the setting still round-trips, we just
/// don't act on it there.
pub fn platform_backend() -> Arc<dyn OutputMuteBackend> {
    #[cfg(target_os = "linux")]
    {
        return Arc::new(linux::PactlBackend::default());
    }
    #[cfg(target_os = "windows")]
    {
        return Arc::new(windows::WindowsBackend::default());
    }
    #[allow(unreachable_code)]
    {
        Arc::new(NoopBackend)
    }
}
