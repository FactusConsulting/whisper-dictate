//! Hotkey installation configuration, errors, and preflight results.
//!
//! This configuration surface is shared by the install funnel
//! ([`super::install`]) and the OS-listener handle ([`super::handle`]),
//! so it stays reviewable on its own.

use super::coordinator::Mode;

#[derive(Debug, Clone)]
pub struct HotkeyConfig {
    pub key_names: Vec<String>,
    pub mode: Mode,
    /// Forwarded to [`coordinator::Options::auto_complete_processing`]. The
    /// diagnostic `hotkey capture` CLI sets this so it doesn't need a
    /// per-cycle `ProcessingFinished` from the sink — the coordinator
    /// completes the stage synchronously as part of the same loop iteration
    /// that emitted `StopAndTranscribe`, before any already-queued next
    /// press is consumed. The shipping supervisor leaves it `false`.
    pub auto_complete_processing: bool,
}

impl HotkeyConfig {
    /// Build a hold-to-talk config from a list of key names. Convenience
    /// for the common case (matches the historical constructor signature
    /// from before the toggle-mode field landed).
    pub fn hold_to_talk(key_names: Vec<String>) -> Self {
        Self {
            key_names,
            mode: Mode::HoldToTalk,
            auto_complete_processing: false,
        }
    }

    /// Fluent setter for [`Self::auto_complete_processing`]. Used by the
    /// diagnostic capture CLI; keeps `HotkeyConfig::hold_to_talk`'s
    /// signature unchanged.
    pub fn with_auto_complete_processing(mut self, on: bool) -> Self {
        self.auto_complete_processing = on;
        self
    }
}

/// Errors from [`install_hotkey`].
///
/// * [`Self::Unsupported`] — the binary was built without the
///   `rust-hotkeys` cargo feature. The supervisor logs a warning and stays
///   on the pynput path.
/// * [`Self::EmptyConfig`] — the PTT binding came in empty.
/// * [`Self::UnsupportedKey`] — a configured key name has no rdev
///   translation (e.g. `super_l`, which the evdev backend accepts but
///   rdev does not). Surfaced so the supervisor can keep the alternate
///   hotkey driver wired.
/// * [`Self::ListenerStartup`] — `rdev::listen` failed at startup (no X
///   display, missing accessibility permission, ...). Surfaced
///   synchronously so the supervisor can fall back to the alternate
///   hotkey driver.
/// * [`Self::AlreadyHeld`] — another whisper-dictate process already owns
///   push-to-talk in this session. Unlike every other variant this is NOT
///   a "fall back to the other backend" signal: falling back would
///   install the very second listener the guard just refused. See
///   [`ptt_lock`] for the interleaved-injection rationale that
///   motivated it.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("rust-hotkeys feature is not compiled in (rebuild with --features rust-hotkeys)")]
    Unsupported,
    #[error("hotkey config is empty (key_names cannot be empty)")]
    EmptyConfig,
    #[error("hotkey key name {0:?} is not supported by the Rust (rdev) backend")]
    UnsupportedKey(String),
    #[error("rdev listener failed to start: {0}")]
    ListenerStartup(String),
    #[error(
        "push-to-talk chord {chord:?} was refused: {holder_desc} already owns push-to-talk \
         in this session. Only one whisper-dictate process may hold it - if two did, one \
         key press would make both record, both transcribe, and both type into the focused \
         window, interleaving the injected text character by character. Quit the other \
         whisper-dictate process, then start this one again."
    )]
    AlreadyHeld {
        /// The `+`-joined chord that was refused.
        chord: String,
        /// PID of the blocking process, when its advisory record was
        /// readable.
        holder_pid: Option<u32>,
        /// Human-readable holder phrase (see
        /// [`ptt_lock::PttConflict::holder_description`]).
        holder_desc: String,
    },
}

/// Convenience alias for the install API.
pub type Result<T> = std::result::Result<T, InstallError>;

/// Side-effect-free description of the listener an install would use.
///
/// This is intentionally separate from [`HotkeyHandle::driver_name`]: a
/// preflight can explain capability and fallback risk before a listener owns
/// the chord, while the handle remains the source of truth after installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyPreflight {
    /// Concrete listener selected for this session (`rdev`, `evdev`, or
    /// `win_registerhotkey`). Never reports the abstract `auto` selector.
    pub planned_driver: &'static str,
    /// Why the requested listener could not express the chord and the install
    /// would use a fallback instead.
    pub fallback_reason: Option<String>,
    /// Windows' low-level rdev hook can be filtered while the GUI is focused.
    /// This is a capability risk, not a claim that a syntactically valid chord
    /// has been observed working on the current machine.
    pub focused_window_risk: bool,
}
