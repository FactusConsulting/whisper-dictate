//! Pure device-selection policy for opening the microphone on a
//! push-to-talk press (#323).
//!
//! * Runtime start validates the configured input by enumeration only; no
//!   stream is opened.
//! * Every press tries the configured input first and falls back to the OS
//!   default input for that recording only, so a temporarily missing USB or
//!   Bluetooth microphone never rewrites the saved choice.
//! * A selector whose open timed out is never retried in this runtime: the
//!   timed-out CPAL worker may still be blocked inside the audio driver, and
//!   retrying would accumulate blocked threads.

#![cfg(feature = "audio-capture")]

use super::capture_status::SYSTEM_DEFAULT_LABEL;

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

/// Remembers which selectors timed out while opening.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenBreaker {
    pub(crate) configured_timed_out: bool,
    pub(crate) default_timed_out: bool,
}

impl OpenBreaker {
    /// Targets a press may still try, in order.
    pub(crate) fn candidates(&self, configured: &str) -> Vec<OpenTarget> {
        let mut targets = Vec::with_capacity(2);
        if has_named_device(configured) && !self.configured_timed_out {
            targets.push(OpenTarget::Configured);
        }
        if !self.default_timed_out {
            targets.push(OpenTarget::SystemDefault);
        }
        targets
    }

    fn record_timeout(&mut self, target: OpenTarget) {
        match target {
            OpenTarget::Configured => self.configured_timed_out = true,
            OpenTarget::SystemDefault => self.default_timed_out = true,
        }
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
    /// True when no candidate remains for later presses (all timed out).
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
            message: "an earlier microphone open timed out inside the audio driver".to_owned(),
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
}

/// Validate that some input exists without opening a stream. Fails only
/// when neither the configured input nor the OS default can be found, which
/// keeps runtime start fail-fast on a machine with no microphone at all.
pub(crate) fn probe_startup_device(
    configured: &str,
    mut probe: impl FnMut(&str) -> Result<(), anyhow::Error>,
) -> Result<StartupDevice, anyhow::Error> {
    if !has_named_device(configured) {
        return probe("").map(|()| StartupDevice::SystemDefault);
    }
    match probe(configured) {
        Ok(()) => Ok(StartupDevice::Configured),
        Err(configured_error) => {
            let configured_error = configured_error.to_string();
            probe("")
                .map(|()| StartupDevice::ConfiguredMissing {
                    error: configured_error.clone(),
                })
                .map_err(|default_error| {
                    anyhow::anyhow!(
                        "could not find configured input ({configured_error}); system default input is also unavailable ({default_error})"
                    )
                })
        }
    }
}

#[cfg(test)]
#[path = "capture_open_policy_tests.rs"]
mod tests;
