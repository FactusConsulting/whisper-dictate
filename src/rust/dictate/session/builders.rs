//! builders for the session-owned utterance lifecycle.

use super::*;

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Enable the configured command hook for production utterances. The hook
    /// resolver reads env/config on every call, so enabling/disabling it in
    /// Settings applies at the next utterance boundary.
    pub fn with_command_hook(mut self) -> Self {
        self.command_hook = true;
        self
    }

    /// Enable command hooks while the shared runtime lifecycle remains active.
    pub fn with_command_hook_activity(mut self, runtime_active: Arc<AtomicBool>) -> Self {
        self.command_hook = true;
        self.command_hook_activity = Some(runtime_active);
        self
    }

    /// Enable hooks from session-owned settings while the runtime remains
    /// active. Used by the in-process runtime so `--config PATH` never falls
    /// through to another config source.
    #[cfg(any(test, all(feature = "whisper-rs-local", feature = "rust-injection")))]
    pub(crate) fn with_owned_command_hook_activity(
        mut self,
        runtime_active: Arc<AtomicBool>,
    ) -> Self {
        self.command_hook = true;
        self.command_hook_uses_owned_settings = true;
        self.command_hook_activity = Some(runtime_active);
        self
    }

    pub(super) fn command_hook_enabled(&self) -> bool {
        let active = self
            .command_hook_activity
            .as_ref()
            .is_none_or(|gate| gate.load(Ordering::Acquire));
        if self.command_hook && !active && crate::diag::debug_enabled() {
            crate::diag::log!(
                "[runtime/debug] command hook suppressed because lifecycle gate is closed"
            );
        }
        self.command_hook && active
    }

    pub(super) fn command_hook_settings(&self) -> Option<(&str, u64)> {
        self.command_hook_uses_owned_settings.then_some((
            self.config.command_hook.as_str(),
            self.config.command_hook_timeout_ms,
        ))
    }

    /// Attach a live-preview engine that will emit `state="preview"` worker
    /// events during recording (see [`preview`] for the cadence + suppression
    /// contract, and [`PreviewEngineConfig::from_seconds`] for the disabled
    /// gate). Passing this is opt-in: the production wiring only attaches an
    /// engine on the LOCAL Whisper backend, matching Python's
    /// `PREVIEW_BACKENDS = ("whisper",)` cloud-cost guard.
    pub fn with_preview_engine(mut self, engine: PreviewEngine) -> Self {
        self.preview = Some(engine);
        self
    }

    /// Attach a preview engine only when one was actually constructed
    /// (i.e. `preview_seconds > 0` AND the backend is preview-eligible).
    /// Convenience wrapper so the runtime factory can call
    /// `.with_optional_preview_engine(...)` without pre-checking the Option.
    pub fn with_optional_preview_engine(mut self, engine: Option<PreviewEngine>) -> Self {
        if let Some(engine) = engine {
            self.preview = Some(engine);
        }
        self
    }

    /// Attach an audio ducker driven at PTT press (start) / PTT release
    /// (stop / cancel). Default is a silent no-op; production wiring
    /// (`make_real_session`) attaches
    /// [`crate::dictate::audio_ducking::SystemAudioDucker`], whose
    /// `from_env` constructor reads `VOICEPI_AUDIO_DUCKING` +
    /// `VOICEPI_AUDIO_DUCKING_LEVEL` (parity with Python's
    /// `vp_audio_ducking.AudioDucker.from_config()`). Closes parity blocker #2.
    pub fn with_ducker(
        mut self,
        ducker: Box<dyn crate::dictate::audio_ducking::AudioDucker + Send>,
    ) -> Self {
        self.audio_ducker = ducker;
        self
    }

    /// Attach an audible-cue sink played at PTT press (start) and PTT
    /// release (stop). The default sink is a silent no-op, so a
    /// caller who does not opt in is byte-for-byte identical to a
    /// session built before this seam existed. Production wiring
    /// (`make_real_session`) attaches
    /// [`crate::dictate::feedback::SystemCueSink`], which itself
    /// respects the `VOICEPI_FEEDBACK_SOUNDS` env-var gate on every
    /// call -- matching Python's `vp_feedback.play_cue`.
    pub fn with_cue_sink(
        mut self,
        sink: Box<dyn crate::dictate::feedback::CueSink + Send>,
    ) -> Self {
        self.cue_sink = sink;
        self
    }

    /// Attach a [`HistorySink`] so every completed utterance also lands in
    /// the local JSONL history file (parity with Python's
    /// `_record_utterance_event -> append_record_sinks`). `None` -- the
    /// default -- keeps the pre-existing no-write behaviour. Sink errors
    /// are non-fatal: the implementation logs a warning and the session
    /// continues, so a broken history file can never drop a dictation.
    ///
    /// Passing this is opt-in: production wiring only attaches a sink when
    /// the user has `history_enabled=true` (default) AND a resolvable
    /// history path -- see [`history_sink_from_settings`].
    pub fn with_history_sink(mut self, sink: Box<dyn HistorySink + Send>) -> Self {
        self.history_sink = Some(sink);
        self
    }

    /// Attach a history sink only when [`history_sink_from_settings`]
    /// resolves one (i.e. the user has not disabled history). Convenience
    /// wrapper so the real-backends factory can call `.with_optional_history_sink()`
    /// without pre-checking the Option itself.
    pub fn with_optional_history_sink(mut self, sink: Option<Box<dyn HistorySink + Send>>) -> Self {
        if let Some(sink) = sink {
            self.history_sink = Some(sink);
        }
        self
    }

    /// Attach a [`MetricsSink`] so every completed utterance also lands
    /// in the machine-readable metrics JSONL file (parity with Python's
    /// `_record_utterance_event -> append_record_sinks` metrics branch).
    /// `None` -- the default -- keeps the pre-existing no-write behaviour.
    /// Sink errors are non-fatal: the implementation logs a warning and the
    /// session continues, so a broken metrics file can never drop a
    /// dictation.
    ///
    /// Passing this is opt-in: production wiring only attaches a sink when
    /// the user has `inject_json=true` (Python `json_output`) AND a
    /// non-empty `metrics_jsonl` path -- see [`metrics_sink_from_settings`].
    pub fn with_metrics_sink(mut self, sink: Box<dyn MetricsSink + Send>) -> Self {
        self.metrics_sink = Some(sink);
        self
    }

    /// Attach a metrics sink only when [`metrics_sink_from_settings`]
    /// resolves one (i.e. the user has `inject_json=true` AND a non-empty
    /// `metrics_jsonl` path). Convenience wrapper so the real-backends
    /// factory can call `.with_optional_metrics_sink()` without pre-checking
    /// the Option itself.
    pub fn with_optional_metrics_sink(mut self, sink: Option<Box<dyn MetricsSink + Send>>) -> Self {
        if let Some(sink) = sink {
            self.metrics_sink = Some(sink);
        }
        self
    }

    /// Attach an LLM post-processing backend, returning the session so
    /// callers can chain it after [`Self::new`]. When set, a successful
    /// utterance runs `backend.post_process(text)` (emitting a
    /// `post-processing` status) before the format-command layer and
    /// injection. Passing this is opt-in: the production wiring only
    /// attaches a backend when the operator configured a post-processor
    /// (`VOICEPI_POST_PROCESSOR` != `none`).
    pub fn with_post_process(mut self, backend: Box<dyn PostProcessBackend + Send>) -> Self {
        self.post_process = Some(backend);
        self
    }

    /// Attach a dictionary whose replacement table rewrites the transcript
    /// BEFORE post-processing, formatting and injection -- mirroring Python's
    /// `_dictionary_runtime(raw_text)` step in `vp_transcribe._transcribe_detail`
    /// (replacements are applied to the decoded text before it leaves the
    /// transcribe path). Passing this is opt-in: the production wiring only
    /// attaches a dictionary when the configured one actually has replacements,
    /// so a session without one is byte-identical to before this seam existed.
    /// (Term-based prompt biasing -- the other half of dictionary support -- is
    /// applied at backend-config construction, not here.)
    pub fn with_dictionary(mut self, dictionary: crate::dictionary::Dictionary) -> Self {
        self.dictionary = Some(Box::new(crate::dictionary::StaticDictionary(dictionary)));
        self
    }

    /// Attach a live-reloading dictionary: the replacement table is re-read
    /// from `config.json` + the process env + the dictionary file(s) at each
    /// utterance boundary (cheap when unchanged, via an mtime+settings cache
    /// key), so a user editing their dictionary or changing the `dictionary*`
    /// live settings sees the change on the next utterance without restarting
    /// the app -- matching Python's per-utterance `_dictionary_runtime` and the
    /// config layer's `live` flag for those keys. `precedence` selects which
    /// source wins: [`crate::dictionary::ReloadPrecedence::ConfigFirst`] for the
    /// live worker session (config.json is the source of truth) or
    /// [`crate::dictionary::ReloadPrecedence::EnvFirst`] for the env-driven
    /// `simulate-session` CLI. The initial table is loaded now, so the first
    /// utterance already reflects the on-disk state. (Term-based prompt biasing
    /// -- the other half of dictionary support -- is still folded into the
    /// backend config at construction; its live-reload is a separate follow-up.)
    pub fn with_reloading_dictionary(
        mut self,
        precedence: crate::dictionary::ReloadPrecedence,
    ) -> Self {
        self.dictionary = Some(Box::new(crate::dictionary::ReloadingDictionary::new(
            precedence,
        )));
        self
    }

    /// Attach a file-reloading dictionary whose settings are owned by this
    /// native session rather than re-read from the process environment.
    #[cfg(any(test, all(feature = "whisper-rs-local", feature = "rust-injection")))]
    pub(crate) fn with_reloading_dictionary_settings(
        mut self,
        settings: crate::dictionary::RuntimeDictionarySettings,
    ) -> Self {
        self.dictionary = Some(Box::new(
            crate::dictionary::ReloadingDictionary::from_settings(settings),
        ));
        self
    }

    /// Attach a session dictionary when it supplies transcript replacements.
    pub fn with_optional_dictionary(
        self,
        dictionary: crate::dictionary::SessionDictionary,
    ) -> Self {
        if dictionary.has_replacements() {
            let mut session = self;
            session.dictionary = Some(Box::new(crate::dictionary::StaticDictionary(
                dictionary.dictionary,
            )));
            session
        } else {
            self
        }
    }
}
