//! profile policy for the session-owned utterance lifecycle.

use super::*;

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Attach a per-utterance target-profile matcher + foreground-window
    /// probe. Parity with the Python worker's
    /// `_capture_target_window` + `_profiled_config` pair (called from
    /// `_start` on every PTT press): each utterance the session probes
    /// the focused window, hands the resulting title / process pair to
    /// the matcher, and applies the returned setting overrides to the
    /// effective [`SessionConfig`] for THIS utterance only.
    ///
    /// Passing this is opt-in: production wiring pairs
    /// [`crate::dictate::profile::ReloadingProfileMatcher`] (re-reads
    /// `config.json` each utterance -- matches Python's
    /// `_reload_live_config_if_changed`) with
    /// [`crate::platform::foreground_window::SystemForegroundWindow`] (the
    /// per-OS probe). Tests plug the
    /// [`crate::dictate::profile::StaticProfileMatcher`] and
    /// [`crate::platform::foreground_window::FixedForegroundWindow`]
    /// deterministic implementations. A session without a matcher stays
    /// byte-identical to one built before this seam existed.
    ///
    /// The session applies `format_commands`, `min_record_seconds`, and
    /// `inject_mode` to the effective configuration. Backend hooks receive
    /// the complete settings map for their own per-utterance overrides.
    pub fn with_profile_matcher(
        mut self,
        matcher: Box<dyn ProfileMatcher>,
        probe: Box<dyn ForegroundWindowProbe>,
    ) -> Self {
        self.profile_matcher = Some(matcher);
        self.foreground_probe = probe;
        self
    }

    /// The profile the matcher resolved for the current / most-recent
    /// utterance. `None` when no matcher is attached, or when the matcher
    /// resolved to no match on the last [`Self::start`] call. Exposed for
    /// telemetry + follow-up backend wiring; the session itself only
    /// consumes the session-config subset.
    pub fn active_profile(&self) -> Option<&AppliedProfile> {
        self.active_profile.as_ref()
    }

    /// Whether a profile matcher has been attached via
    /// [`Self::with_profile_matcher`]. Exposed so the production factory's
    /// unit tests can assert the wire-up (Codex P1 #607) without the
    /// session having to leak its private matcher field.
    pub fn has_profile_matcher(&self) -> bool {
        self.profile_matcher.is_some()
    }

    /// Apply the resolved [`AppliedProfile`] to the effective
    /// [`SessionConfig`] for this utterance. Called from [`Self::start`]
    /// after probing + matching. Split out so the mapping between
    /// profile-setting keys and `SessionConfig` fields is easy to unit
    /// test and to extend.
    ///
    /// Reset semantics: `self.config` is refreshed from `self.base_config`
    /// first so a profile that fired for a PREVIOUS utterance cannot leak
    /// its overrides into the current one. Then each supported key from
    /// the profile settings map overwrites the matching field.
    ///
    /// Invalid numeric overrides use the shared schema default before they
    /// reach the session or its backends. Other unsupported values retain
    /// their existing fall-through behavior.
    pub(super) fn apply_active_profile(&mut self) {
        self.config = self.base_config.clone();
        // Empty map when no profile matched -- the backends need this so
        // they can RESET any overrides they applied for a PREVIOUS
        // utterance's profile (else a profile that fired for utterance N
        // would silently persist into N+1 when N+1 hits the wildcard /
        // default branch). Mirrors the config-reset done above for
        // `self.config`.
        let mut effective_settings = self.live_settings.clone();
        if let Some(profile) = self.active_profile.as_ref() {
            effective_settings.extend(profile.settings.clone());
        }
        for (key, value) in &mut effective_settings {
            *value = crate::config::numeric::runtime_value(key, std::mem::take(value));
        }
        if let Some(processor) = effective_settings.get("post_processor").cloned() {
            if let Some(model) = effective_settings.get_mut("post_model") {
                crate::config::normalize_groq_post_model(&processor, model);
            }
        }
        let settings = &effective_settings;
        if let Some(value) = settings.get("format_commands") {
            let trimmed = value.trim();
            self.config.format_command_set = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            };
        }
        if let Some(value) = settings.get("min_record_seconds") {
            if let Ok(parsed) = value.trim().parse::<f64>() {
                self.config.min_record_seconds = parsed;
            }
        }
        if let Some(value) = settings.get("max_record_s") {
            if let Some(parsed) = parse_max_record_seconds(value) {
                self.config.max_record_seconds = Some(parsed);
            }
        }
        if let Some(value) = settings.get("inject_mode") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                self.config.inject_mode = trimmed.to_owned();
            }
        }
        if let Some(value) = settings.get("command_hook") {
            self.config.command_hook = value.clone();
        }
        if let Some(value) = settings.get("command_hook_timeout_ms") {
            if let Ok(parsed) = value.trim().parse::<f64>() {
                if parsed.is_finite() {
                    self.config.command_hook_timeout_ms = parsed.max(1.0) as u64;
                }
            }
        }
        // Each backend picks the keys it understands and stores them for its
        // next call without rebuilding the session.
        self.transcribe.apply_profile_overrides(settings);
        self.inject.apply_profile_overrides(settings);
        self.cue_sink.apply_settings(settings);
        if let Some(dictionary) = self.dictionary.as_mut() {
            dictionary.apply_settings(settings);
        }
        if let Some(backend) = self.post_process.as_ref() {
            backend.apply_profile_overrides(settings);
        }
    }

    /// Probe the foreground window + resolve the matched profile. Split
    /// out so tests can drive the resolve path without going through
    /// `start()` (which owns the event-emission side effects).
    pub(super) fn resolve_profile(&self) -> (WindowInfo, Option<AppliedProfile>) {
        let Some(matcher) = self.profile_matcher.as_ref() else {
            return (WindowInfo::default(), None);
        };
        let window = self.foreground_probe.probe();
        let applied = matcher.resolve(&window);
        if applied.is_none() {
            (window, None)
        } else {
            (window, Some(applied))
        }
    }
}

/// Translate the backend's free-form gate text (as `result.gate` carries
/// it -- e.g. `"input too quiet: -42 dBFS"`, `"no speech contrast: 0.02"`)
/// into one of the three reason tokens the worker-event consumers / UI
/// cards switch on: `"too_quiet"`, `"no_speech"`, `"empty"`. Mirrors the
/// Python mapper in `vp_transcribe.py` (substring-based, ASCII-cased).
/// Codex P2 #413 mod.rs:284 (round 2 follow-up to the `gate` field
/// landed in round 1).
/// Emit one `[worker-event] event=status state=profile` line describing
/// the profile match resolved at the top of [`DictateSession::start`].
/// Mirrors the Python worker's `[profile] active: NAME` print in
/// `vp_dictate._profiled_config` -- the JSON form so consumers (egui log
/// card, telemetry, tests) key off the same wire shape they already read
/// for `recording` / `transcribing`.
///
/// Emitted for every utterance where a profile matcher is attached
/// (whether or not a profile actually fired) so the observer sees the
/// negative case too (`active_profile=""`), matching Python's
/// `f"[profile] active: {profile_name or 'default'}"`. The line is
/// suppressed entirely when no matcher is attached so tests that
/// pre-date this seam keep their exact event traces.
pub(super) fn emit_profile_status<W: Write>(
    writer: &mut W,
    window: &WindowInfo,
    applied: Option<&AppliedProfile>,
    output: crate::dictate::events::WorkerEventOutput,
) -> Result<(), SessionError> {
    let profile_name = applied
        .and_then(|p| p.name.as_deref())
        .unwrap_or_default()
        .to_owned();
    let extras: [(&'static str, Value); 4] = [
        ("active_profile", Value::from(profile_name)),
        (
            "target_title",
            Value::from(window.title.clone().unwrap_or_default()),
        ),
        (
            "target_process",
            Value::from(window.process.clone().unwrap_or_default()),
        ),
        (
            "target_id",
            Value::from(window.target_id.clone().unwrap_or_default()),
        ),
    ];
    wire::emit_status_with_output(writer, "profile", &extras, output)
}
