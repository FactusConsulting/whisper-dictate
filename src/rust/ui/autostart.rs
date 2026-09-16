//! Auto-start the dictation runtime on app launch (`ui_autostart_runtime`).
//!
//! Three deliberate properties, all of them requirements from #894:
//!
//! - **One shot, never a retry loop.** The session latch
//!   ([`AutostartStage`]) is consumed on the frame auto-start is due,
//!   BEFORE anything can fail, so a failed start leaves the runtime stopped
//!   with the reason in the runtime log (exactly as a manual Start does) and
//!   the Start button available. It cannot spin, and toggling the preference
//!   on mid-session does not retroactively fire it — the toggle applies at
//!   the next launch.
//! - **Not from `App::new`.** It fires on the frame AFTER the first painted
//!   one, so the window is up and the runtime log is visible when the start
//!   (or the skip reason) is reported.
//! - **Never on an unconfigured install.** A cloud provider with no API key
//!   or a local model that isn't downloaded is a SKIP with a logged reason,
//!   not a start attempt — a first-run user must not be met with an error
//!   they never asked for.

use super::*;

/// Session-only progress of the launch-time auto-start one-shot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::ui) enum AutostartStage {
    /// No frame has been rendered yet — the window may not even be mapped.
    #[default]
    BeforeFirstFrame,
    /// The first frame is painted; auto-start is due on the next pass.
    FirstFrameRendered,
    /// The one-shot has been consumed. Never fires again this session,
    /// whatever the outcome was.
    Done,
}

impl AutostartStage {
    /// Advance one render pass. Returns the next stage plus whether THIS
    /// pass is the one that should attempt auto-start.
    ///
    /// Pure so the "exactly once, on the second frame, never again" rule is
    /// unit-testable without a renderer or a runtime supervisor.
    pub(in crate::ui) fn advance(self) -> (Self, bool) {
        match self {
            Self::BeforeFirstFrame => (Self::FirstFrameRendered, false),
            Self::FirstFrameRendered => (Self::Done, true),
            Self::Done => (Self::Done, false),
        }
    }
}

/// Why auto-start must be skipped on this install, or `None` when the saved
/// configuration is usable.
///
/// Pure (all three inputs are already-computed diagnostics from the same
/// helpers the Start button uses — `cloud_stt_missing_api_key_message`,
/// `runtime_whisper_model_warning`, and
/// `AppSettings::validate_nemotron_profile_language`) so the skip policy is
/// testable without an API key store or a model cache. Cloud key first, then
/// the local-model download, then the Nemotron profile/language mismatch —
/// the exact order does not matter much (#894 only asks for ONE actionable
/// reason), but it is stable so the log line is deterministic.
pub(in crate::ui) fn autostart_skip_reason(
    missing_cloud_key: Option<&str>,
    local_model_warning: Option<&str>,
    nemotron_profile_language_error: Option<&str>,
) -> Option<String> {
    missing_cloud_key
        .or(local_model_warning)
        .or(nemotron_profile_language_error)
        .map(str::to_owned)
}

impl WhisperDictateApp {
    /// Drive the launch-time auto-start one-shot. Called once per render
    /// pass from `App::ui`.
    pub(in crate::ui) fn poll_autostart_runtime(&mut self) {
        let (next, due) = self.autostart_stage.advance();
        self.autostart_stage = next;
        if !due {
            return;
        }
        // Defensive belt-and-braces: nothing in the app reaches this point
        // with the runtime already running or mid-restart today (the one-
        // shot fires once, before any Start/Stop click is even possible),
        // but if that ever changed, calling `start_runtime` on a healthy
        // running worker would raise a spurious red error banner rather
        // than a genuine failure. Silent no-op, not a skip worth logging —
        // there is nothing wrong with the configuration to report.
        if self.supervisor.is_running_or_restarting() {
            return;
        }
        // Read the SAVED preference, not the in-memory one: this is a
        // launch-time decision about what is on disk, and an unsaved click
        // on the toggle already persisted itself through
        // `set_autostart_runtime`.
        if !self.saved_settings.ui_autostart_runtime {
            return;
        }
        // Loading the cloud key is what makes `cloud_stt_missing_api_key`
        // meaningful; `start_runtime` does the same before its own check.
        self.ensure_stt_api_key_loaded_for_runtime();
        let missing_cloud_key = self
            .cloud_stt_missing_api_key()
            .then(|| self.cloud_stt_missing_api_key_message());
        let local_model_warning = self.runtime_whisper_model_warning();
        // Mirrors `start_runtime`'s own `validate_nemotron_profile_language_for_runtime`
        // gate (#894's "must not auto-start an unconfigured install" also
        // covers this: an English-Nemotron-profile-with-non-English-language
        // mismatch is exactly as unusable as a missing key or model, and
        // without this check every launch would otherwise raise the same
        // banner `start_runtime` raises, via the side-effect-free
        // `nemotron_profile_language_error` rather than the banner-raising
        // `validate_nemotron_profile_language_for_runtime`.
        let nemotron_profile_language_error = self.nemotron_profile_language_error();
        if let Some(reason) = autostart_skip_reason(
            missing_cloud_key.as_deref(),
            local_model_warning.as_deref(),
            nemotron_profile_language_error.as_deref(),
        ) {
            // A log line only: deliberately NOT `last_runtime_error` /
            // `settings_status`, which would put a red banner in front of a
            // first-run user who never pressed Start.
            self.append_runtime_log(format!(
                "[ui] auto-start skipped, the saved configuration is not usable yet: {reason}"
            ));
            return;
        }
        self.append_runtime_log(
            "[ui] auto-start: starting the runtime from the saved settings \
             (System -> Startup to turn this off)",
        );
        // The SAME path as the Start button, including every one of its own
        // guards and its failure reporting. A failure is reported there and
        // then left alone: the stage is already `Done`, so nothing retries.
        self.start_runtime();
    }

    /// Apply and persist the auto-start preference the moment the checkbox is
    /// clicked.
    ///
    /// Writes ONLY `ui_autostart_runtime`, through
    /// [`config::set_raw_string_key`], for exactly the reason
    /// `set_settings_mode` does (see its doc comment): routing a single
    /// key through [`config::set_value`] materializes every OTHER known
    /// setting's schema default into a sparse config.json, which silently
    /// clobbers env-only overrides such as `VOICEPI_LOCAL_ONLY=1` (#890's
    /// class of bug). Booleans are stored in the config file as the strings
    /// `"1"` / `"0"` (see `set_bool` in `config/save.rs`), so the raw write
    /// uses the same encoding a full save would.
    ///
    /// `saved_settings` is updated only AFTER a successful write, so a failed
    /// save leaves the checkbox correctly reporting the value as still
    /// pending rather than looking clean.
    pub(in crate::ui) fn set_autostart_runtime(&mut self, enabled: bool) {
        self.settings.ui_autostart_runtime = enabled;
        let raw = if enabled { "1" } else { "0" };
        match config::set_raw_string_key("ui_autostart_runtime", raw, &config::config_path()) {
            Ok(_) => self.saved_settings.ui_autostart_runtime = enabled,
            Err(err) => {
                self.append_runtime_log(format!(
                    "[ui] could not persist the auto-start preference: {err}"
                ));
                self.settings_status = format!("Could not save the auto-start setting: {err}.");
            }
        }
    }
}
