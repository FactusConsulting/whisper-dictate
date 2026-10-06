//! model policy responsibilities for the desktop controller.

use super::*;

enum ExternalWhisperModelPath {
    Unset,
    Valid,
    Invalid,
}

impl WhisperDictateApp {
    fn external_whisper_model_path(&self) -> ExternalWhisperModelPath {
        match std::env::var(WHISPER_MODEL_PATH_ENV) {
            Err(std::env::VarError::NotPresent) => ExternalWhisperModelPath::Unset,
            Err(std::env::VarError::NotUnicode(_)) => ExternalWhisperModelPath::Invalid,
            Ok(path) if std::path::Path::new(path.trim()).is_file() => {
                ExternalWhisperModelPath::Valid
            }
            Ok(_) => ExternalWhisperModelPath::Invalid,
        }
    }

    pub(in crate::ui) fn has_external_whisper_model_path(&self) -> bool {
        matches!(
            self.external_whisper_model_path(),
            ExternalWhisperModelPath::Valid
        )
    }

    pub(in crate::ui) fn selected_whisper_model_warning(&self) -> Option<String> {
        self.whisper_model_warning(&self.settings.stt_backend, &self.settings.model)
    }

    pub(in crate::ui) fn runtime_whisper_model_warning(&self) -> Option<String> {
        let ambient_env = crate::runtime::in_process::ambient_session_env();
        let command = crate::runtime::default_worker_command_with_ambient_env(&ambient_env);
        let effective = |name: &str, fallback: &str| {
            command
                .runtime
                .value(name)
                .map(str::to_owned)
                .unwrap_or_else(|| fallback.to_owned())
        };
        let stt_backend = effective("VOICEPI_STT_BACKEND", &self.saved_settings.stt_backend);
        let model = effective("VOICEPI_MODEL", &self.saved_settings.model);
        self.whisper_model_warning(&stt_backend, &model)
    }

    fn whisper_model_warning(&self, stt_backend: &str, model: &str) -> Option<String> {
        if SttBackendMode::from_raw(stt_backend) != SttBackendMode::Whisper {
            return None;
        }

        match self.external_whisper_model_path() {
            ExternalWhisperModelPath::Valid => return None,
            ExternalWhisperModelPath::Invalid => {
                return Some(format!(
                    "{WHISPER_MODEL_PATH_ENV} must point to an existing GGML model file."
                ));
            }
            ExternalWhisperModelPath::Unset => {}
        }

        let Some(entry) = crate::whisper::model_manager::find(model) else {
            return Some(format!(
                "{} is not supported. Choose a listed model before recording.",
                model
            ));
        };
        let visible_in_picker = crate::whisper::model_manager::visible_catalog()
            .any(|candidate| candidate.name == entry.name);
        let local_only = self.local_only_downloads_blocked();
        let Some(availability) = self.whisper_model_downloads.cached_availability(entry) else {
            let availability = if crate::whisper::model_manager::is_downloaded(entry) {
                crate::ui::whisper_models_state::ModelAvailability::Available
            } else {
                crate::ui::whisper_models_state::ModelAvailability::Missing
            };
            return model_download_warning(model, availability, visible_in_picker, local_only);
        };
        model_download_warning(model, availability, visible_in_picker, local_only)
    }

    pub(in crate::ui) fn local_only_enabled(&self) -> bool {
        if self.supervisor.is_running_or_restarting() {
            return self.supervisor.active_local_only().unwrap_or(false);
        }
        self.desired_local_only()
    }

    pub(in crate::ui) fn local_only_change_pending(&self) -> bool {
        self.supervisor.is_running_or_restarting()
            && self.supervisor.active_local_only().unwrap_or(false) != self.desired_local_only()
    }

    pub(in crate::ui) fn desired_local_only(&self) -> bool {
        self.saved_settings.local_only || self.local_only_ambient_env_override()
    }

    /// Whether `VOICEPI_LOCAL_ONLY` is currently set (in this process's real
    /// environment) to a truthy value. Split out of [`Self::desired_local_only`]
    /// and made visible to callers that need to explain WHY local-only is
    /// active, not just whether: pointing a user at the `local_only` Settings
    /// toggle (hidden in Simple mode, and requiring Advanced -> System ->
    /// Integration to reach even in Advanced) does not help when an ambient
    /// environment override is what's actually blocking them — the toggle
    /// only touches the persisted setting, never the environment.
    pub(in crate::ui) fn local_only_ambient_env_override(&self) -> bool {
        let ambient_env = crate::runtime::in_process::ambient_session_env();
        ambient_env
            .get("VOICEPI_LOCAL_ONLY")
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "True" | "TRUE"))
    }

    /// Return the privacy state that should govern new model transfers.
    ///
    /// A requested change is not active until the runtime restarts. When the
    /// user is turning local-only mode off, however, downloads must be
    /// available so a missing model can be repaired and the restart can
    /// complete; the running session remains unchanged until then.
    pub(in crate::ui) fn local_only_downloads_blocked(&self) -> bool {
        if self.local_only_change_pending() {
            return self.desired_local_only();
        }
        self.local_only_enabled()
    }
}
