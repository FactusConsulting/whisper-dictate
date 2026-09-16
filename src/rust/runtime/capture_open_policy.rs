//! Pure device-selection policy for opening the microphone on a
//! push-to-talk press (#323).
//!
//! * Runtime start only checks, by enumeration, whether an input exists. It
//!   never opens a stream and never refuses to start: a microphone plugged in
//!   later is used on the next press.
//! * Every press tries the configured input first and falls back to the OS
//!   default input for that recording only, so a temporarily missing USB or
//!   Bluetooth microphone never rewrites the saved choice.
//! * A selector whose open timed out [`OPEN_TIMEOUT_STRIKES`] times is never
//!   retried in this runtime: a timed-out CPAL worker may still be blocked
//!   inside the audio driver, and unbounded retries would accumulate blocked
//!   threads. One retry is allowed so a first-use OS permission prompt that
//!   outlives the 5 s start timeout cannot disable the microphone for good.

#![cfg(feature = "audio-capture")]
// The production caller (`rust_session_audio`) needs the full backend set.
#![cfg_attr(
    not(all(feature = "whisper-rs-local", feature = "rust-injection")),
    allow(dead_code)
)]

use super::capture_status::SYSTEM_DEFAULT_LABEL;

/// Timeouts a selector may accumulate before it is skipped for the rest of
/// the runtime.
pub(crate) const OPEN_TIMEOUT_STRIKES: u8 = 2;

/// Which input an open attempt targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenTarget {
    Configured,
    SystemDefault,
}

impl OpenTarget {
    /// CPAL selector for this target. An empty selector is the default-host
    /// default input on Windows, Linux and macOS.
    pub(crate) fn selector(self, configured: &str) -> &str {
        match self {
            Self::Configured => configured,
            Self::SystemDefault => "",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Configured => "configured input",
            Self::SystemDefault => "system default input",
        }
    }

    /// Label published as the effective audio device.
    pub(crate) fn effective_label(self, configured: &str) -> &str {
        match self {
            Self::Configured => configured,
            Self::SystemDefault => SYSTEM_DEFAULT_LABEL,
        }
    }
}

/// A named saved device can be missing while the OS default still works. An
/// empty selector already is the OS default.
pub(crate) fn has_named_device(configured: &str) -> bool {
    !configured.trim().is_empty()
}

/// Counts open timeouts per target.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenBreaker {
    configured_timeouts: u8,
    default_timeouts: u8,
}

impl OpenBreaker {
    /// Targets a press may still try, in order.
    pub(crate) fn candidates(&self, configured: &str) -> Vec<OpenTarget> {
        let mut targets = Vec::with_capacity(2);
        if has_named_device(configured) && self.configured_timeouts < OPEN_TIMEOUT_STRIKES {
            targets.push(OpenTarget::Configured);
        }
        if self.default_timeouts < OPEN_TIMEOUT_STRIKES {
            targets.push(OpenTarget::SystemDefault);
        }
        targets
    }

    #[cfg(test)]
    pub(crate) fn timeouts(&self, target: OpenTarget) -> u8 {
        match target {
            OpenTarget::Configured => self.configured_timeouts,
            OpenTarget::SystemDefault => self.default_timeouts,
        }
    }

    fn record_timeout(&mut self, target: OpenTarget) {
        let count = match target {
            OpenTarget::Configured => &mut self.configured_timeouts,
            OpenTarget::SystemDefault => &mut self.default_timeouts,
        };
        *count = count.saturating_add(1);
    }
}

/// A stream opened for one recording.
#[derive(Debug)]
pub(crate) struct OpenedCapture<S> {
    pub(crate) stream: S,
    pub(crate) target: OpenTarget,
    /// Why the configured input was skipped, when the default was used.
    pub(crate) configured_error: Option<String>,
}

/// Every candidate failed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OpenFailure {
    pub(crate) message: String,
    /// True when no candidate remains for later presses (all timed out
    /// [`OPEN_TIMEOUT_STRIKES`] times).
    pub(crate) paused: bool,
}

/// Try each remaining candidate once, recording timeouts in `breaker`.
pub(crate) fn open_with_fallback<S>(
    configured: &str,
    breaker: &mut OpenBreaker,
    mut open: impl FnMut(&str) -> Result<S, anyhow::Error>,
    is_timeout: impl Fn(&anyhow::Error) -> bool,
) -> Result<OpenedCapture<S>, OpenFailure> {
    let candidates = breaker.candidates(configured);
    if candidates.is_empty() {
        return Err(OpenFailure {
            message: format!(
                "the microphone open timed out inside the audio driver {OPEN_TIMEOUT_STRIKES} times"
            ),
            paused: true,
        });
    }
    let mut configured_error = None;
    let mut errors = Vec::with_capacity(candidates.len());
    for target in candidates {
        match open(target.selector(configured)) {
            Ok(stream) => {
                return Ok(OpenedCapture {
                    stream,
                    target,
                    configured_error,
                })
            }
            Err(error) => {
                if is_timeout(&error) {
                    breaker.record_timeout(target);
                }
                let text = error.to_string();
                errors.push(format!("{} ({text})", target.description()));
                if target == OpenTarget::Configured {
                    configured_error = Some(text);
                }
            }
        }
    }
    Err(OpenFailure {
        message: errors.join("; "),
        paused: breaker.candidates(configured).is_empty(),
    })
}

/// Outcome of the enumeration-only device check at runtime start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StartupDevice {
    Configured,
    SystemDefault,
    /// The configured input is missing but the OS default exists.
    ConfiguredMissing {
        error: String,
    },
    /// No input device exists right now.
    NoInput {
        error: String,
    },
}

/// Check which input exists without opening a stream. Never fails: with no
/// input at all the runtime still starts and reports it on the next press.
pub(crate) fn probe_startup_device(
    configured: &str,
    mut probe: impl FnMut(&str) -> Result<(), anyhow::Error>,
) -> StartupDevice {
    if !has_named_device(configured) {
        return match probe("") {
            Ok(()) => StartupDevice::SystemDefault,
            Err(error) => StartupDevice::NoInput {
                error: error.to_string(),
            },
        };
    }
    let configured_error = match probe(configured) {
        Ok(()) => return StartupDevice::Configured,
        Err(error) => error.to_string(),
    };
    match probe("") {
        Ok(()) => StartupDevice::ConfiguredMissing {
            error: configured_error,
        },
        Err(default_error) => StartupDevice::NoInput {
            error: format!(
                "configured input ({configured_error}); system default input ({default_error})"
            ),
        },
    }
}

#[cfg(test)]
#[path = "capture_open_policy_tests.rs"]
mod tests;
