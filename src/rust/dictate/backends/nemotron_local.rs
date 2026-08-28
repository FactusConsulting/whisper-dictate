//! In-process Nemotron 3.5 backend backed by the NeMo-Speech.cpp C ABI.
//!
//! This is deliberately separate from [`super::cloud_transcribe`].  A local
//! GGUF model is decoded in this process and never needs an API key, Docker,
//! NIM, or an HTTP/gRPC listener.  The same speech gate, dictionary reload,
//! hallucination filter, and profile language overrides used by local Whisper
//! are retained here so switching execution mode does not change the session
//! contract.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};

use super::hallucination::{finalize_transcript, TranscriptionGuards};
use super::nemotron_assets::{
    ensure_library_path, ensure_model_path, library_path_for_request, model_path_for_request,
};
use super::nemotron_ffi::NativeRecognizer;
use crate::dictate::{TranscribeBackend, TranscribeError, TranscribeResult};
use crate::whisper::IdleUnloadingModel;

/// Settings needed to construct the in-process recognizer.  The model and
/// library paths are owned so idle unload/reload can recreate the recognizer
/// without consulting process environment or a UI object.
#[derive(Debug, Clone)]
pub struct NemotronLocalBackendConfig {
    pub model_path: PathBuf,
    pub library_path: PathBuf,
    pub gpu: i32,
    pub accel_label: &'static str,
    pub language: Option<String>,
    pub initial_prompt: Option<String>,
    pub(crate) local_only: bool,
    pub(crate) model_request: String,
    pub(crate) library_override: Option<String>,
    pub(crate) device: String,
}

/// Production Nemotron backend.  `NativeRecognizer` is created lazily on the
/// first utterance and can be unloaded after the shared Whisper idle timeout.
pub struct NemotronLocalTranscribeBackend {
    model: Arc<IdleUnloadingModel<NativeRecognizer>>,
    config: NemotronLocalBackendConfig,
    resolved_accel: Arc<Mutex<&'static str>>,
    prompt_reload: Option<Box<Mutex<crate::dictionary::ReloadingDictionary>>>,
    language_override: Arc<Mutex<Option<Option<String>>>>,
    profile_prompt: Mutex<Option<String>>,
    transcription_guards: Arc<Mutex<Option<TranscriptionGuards>>>,
}

impl NemotronLocalTranscribeBackend {
    pub fn new(
        config: NemotronLocalBackendConfig,
        idle_timeout: Option<std::time::Duration>,
    ) -> Self {
        let config_for_loader = config.clone();
        let resolved_accel = Arc::new(Mutex::new(config.accel_label));
        let resolved_accel_for_loader = Arc::clone(&resolved_accel);
        let model = IdleUnloadingModel::new(
            move || {
                let (recognizer, accel) = load_native_recognizer(&config_for_loader)?;
                *resolved_accel_for_loader
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = accel;
                Ok(recognizer)
            },
            idle_timeout,
        );
        Self {
            model: Arc::new(model),
            config,
            resolved_accel,
            prompt_reload: None,
            language_override: Arc::new(Mutex::new(None)),
            profile_prompt: Mutex::new(None),
            transcription_guards: Arc::new(Mutex::new(None)),
        }
    }

    /// Probe the native runtime and model without recording.  The C ABI loads
    /// the GGUF weights during recognizer creation, so a successful return is
    /// a meaningful local equivalent of a cloud API check.
    pub fn check_configuration(config: &NemotronLocalBackendConfig) -> Result<()> {
        let (_recognizer, _accel) = load_native_recognizer(config)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn with_reloading_prompt_settings(
        mut self,
        settings: crate::dictionary::RuntimeDictionarySettings,
    ) -> Self {
        self.prompt_reload = Some(Box::new(Mutex::new(
            crate::dictionary::ReloadingDictionary::from_settings(settings),
        )));
        self
    }

    #[allow(dead_code)]
    pub(crate) fn with_transcription_guards(mut self, guards: TranscriptionGuards) -> Self {
        self.transcription_guards = Arc::new(Mutex::new(Some(guards)));
        self
    }

    fn effective_guards(&self) -> TranscriptionGuards {
        self.transcription_guards
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .unwrap_or_else(TranscriptionGuards::from_env)
    }

    fn effective_language(&self) -> Option<String> {
        let live = self
            .language_override
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let configured = live
            .unwrap_or_else(|| self.config.language.clone())
            .filter(|value| !value.trim().is_empty());
        configured.map(|value| language_for_model(&value, &self.config.model_path))
    }

    fn effective_prompt(&self) -> (Option<String>, Vec<String>) {
        if let Some(profile) = self
            .profile_prompt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return (Some(profile), Vec::new());
        }
        match &self.prompt_reload {
            Some(reload) => reload
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .initial_prompt_with_terms(self.config.initial_prompt.as_deref()),
            None => (self.config.initial_prompt.clone(), Vec::new()),
        }
    }

    pub fn config(&self) -> &NemotronLocalBackendConfig {
        &self.config
    }
}

impl TranscribeBackend for NemotronLocalTranscribeBackend {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        let guards = self.effective_guards();
        let (audio, duration_s) =
            match crate::audio_dsp::prepare_for_transcription(pcm, sample_rate, &guards.thresholds)
            {
                crate::audio_dsp::PreparedAudio::Reject { reason, duration_s } => {
                    return Ok(TranscribeResult {
                        text: String::new(),
                        gate: Some(reason),
                        duration_s,
                        ..Default::default()
                    });
                }
                crate::audio_dsp::PreparedAudio::Decode { audio, duration_s } => {
                    (audio, duration_s)
                }
            };
        let language = self.effective_language();
        let request_language = language.as_deref().unwrap_or("auto");
        let (prompt, dictionary_terms) = self.effective_prompt();
        let prompt = prompt.as_deref().filter(|value| !value.trim().is_empty());
        let start = Instant::now();
        let result = self
            .model
            .with_model(|recognizer| {
                recognizer.recognize(
                    &audio,
                    sample_rate,
                    request_language,
                    prompt,
                    &dictionary_terms,
                )
            })
            .map_err(|error| TranscribeError::Backend(format!("{error:#}")))?;
        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (text, is_hallucination) =
            finalize_transcript(&result.text, duration_s, guards.max_chars_per_second);
        let stt_accel = self
            .resolved_accel
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .to_owned();
        Ok(TranscribeResult {
            raw_text: result.text,
            dictionary_terms: (!dictionary_terms.is_empty())
                .then(|| dictionary_terms.into_boxed_slice()),
            text,
            is_hallucination,
            latency_ms,
            duration_s,
            language: result
                .language
                .or_else(|| language.and_then(|value| language_result_label(&value)))
                .unwrap_or_default(),
            stt_impl: crate::dictate::provenance::STT_IMPL_NEMOTRON_LOCAL.to_owned(),
            stt_accel,
            ..Default::default()
        })
    }

    fn apply_profile_overrides(&self, settings: &std::collections::BTreeMap<String, String>) {
        if let Some(guards) = self
            .transcription_guards
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            guards.apply_settings(settings);
        }
        if let Some(reload) = self.prompt_reload.as_ref() {
            crate::dictionary::DictionaryProvider::apply_settings(
                &mut *reload
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
                settings,
            );
        }
        let prompt_override = settings
            .get("initial_prompt")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        *self
            .profile_prompt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = prompt_override;
        let language_override = settings
            .get("language")
            .or_else(|| settings.get("lang"))
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let supplied = settings.contains_key("language") || settings.contains_key("lang");
        *self
            .language_override
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            supplied.then_some(language_override);
    }
}

/// Map the compact values persisted by the settings UI to the regional codes
/// expected by the multilingual Nemotron checkpoint.  `auto` is explicit so
/// the model performs language identification rather than inheriting a stale
/// process-level locale.
fn language_for_model(language: &str, model_path: &Path) -> String {
    let raw = language.trim().replace('_', "-");
    if is_english_model_path(model_path)
        && !raw.is_empty()
        && !raw.eq_ignore_ascii_case("auto")
        && !raw.eq_ignore_ascii_case("multi")
        && !raw.eq_ignore_ascii_case("en")
        && !raw.to_ascii_lowercase().starts_with("en-")
    {
        return "en-US".to_owned();
    }
    if raw.is_empty() || raw.eq_ignore_ascii_case("auto") || raw.eq_ignore_ascii_case("multi") {
        return if is_english_model_path(model_path) {
            "en-US".to_owned()
        } else {
            "auto".to_owned()
        };
    }
    if raw.contains('-') {
        return raw;
    }
    let lower = raw.to_ascii_lowercase();
    crate::dictate::backends::cloud_transcribe::NEMOTRON_MULTI_LANGUAGE_LOCALES
        .iter()
        .find_map(|locale| {
            locale
                .split_once('-')
                .filter(|(language, _)| language.eq_ignore_ascii_case(&lower))
                .map(|_| (*locale).to_owned())
        })
        .unwrap_or(raw)
}

fn language_result_label(language: &str) -> Option<String> {
    let compact = language.split('-').next().unwrap_or(language).trim();
    (!compact.is_empty()
        && !compact.eq_ignore_ascii_case("auto")
        && !compact.eq_ignore_ascii_case("multi"))
    .then(|| compact.to_owned())
}

fn is_english_model_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase().contains("speech-streaming-en"))
        .unwrap_or(false)
}

/// Build the local configuration from the existing Nemotron Speech-tab
/// fields. `stt_model` accepts an official model id (resolved to the verified
/// per-user cache) or an existing GGUF path in `inproc://nemotron` mode; cloud
/// Nemotron continues to use its model id unchanged.
pub(crate) fn config_from_settings(
    model: &str,
    device: &str,
    language: Option<String>,
    initial_prompt: Option<String>,
    library_override: Option<&str>,
    local_only: bool,
) -> Result<NemotronLocalBackendConfig> {
    let model_request = model.trim().to_owned();
    let library_override = library_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let model_path = model_path_for_request(&model_request)?;
    let library_path = library_path_for_request(library_override.as_deref(), device)?;
    let device = device.trim().to_owned();
    let (gpu, accel_label) = match device.trim().to_ascii_lowercase().as_str() {
        "cpu" => (-1, "cpu"),
        "cuda" => (0, "cuda"),
        "vulkan" => (0, "vulkan"),
        _ => (0, "unknown"),
    };
    Ok(NemotronLocalBackendConfig {
        model_path,
        library_path,
        gpu,
        accel_label,
        language,
        initial_prompt,
        local_only,
        model_request,
        library_override,
        device,
    })
}

fn load_native_recognizer(
    config: &NemotronLocalBackendConfig,
) -> Result<(NativeRecognizer, &'static str)> {
    let model_path = ensure_model_path(&config.model_request, config.local_only)?;
    let primary_accel = primary_accel_label(&config.device, config.accel_label);
    let primary = || -> Result<(NativeRecognizer, &'static str)> {
        let library_path = ensure_library_path(
            config.library_override.as_deref(),
            &config.device,
            config.local_only,
        )?;
        NativeRecognizer::new(&library_path, &model_path, config.gpu)
            .map(|recognizer| (recognizer, primary_accel))
    };
    if !config.device.eq_ignore_ascii_case("auto") {
        return primary();
    }
    match primary() {
        Ok(recognizer) => Ok(recognizer),
        Err(primary_error) => {
            crate::diag::log!(
                "[nemotron] auto accelerator unavailable ({primary_error:#}); retrying with CPU"
            );
            let library_path =
                ensure_library_path(config.library_override.as_deref(), "cpu", config.local_only)?;
            NativeRecognizer::new(&library_path, &model_path, -1)
                .map(|recognizer| (recognizer, "cpu"))
                .with_context(|| {
                    format!(
                        "Nemotron auto accelerator failed ({primary_error:#}); CPU fallback failed"
                    )
                })
        }
    }
}

fn primary_accel_label(device: &str, configured: &'static str) -> &'static str {
    if device.eq_ignore_ascii_case("auto") {
        // `auto` selects the Vulkan archive first; only the recognizer load
        // can tell us whether the subsequent CPU fallback was required.
        "vulkan"
    } else {
        configured
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_sent_to_multilingual_model_explicitly() {
        let path = Path::new("nemotron-3.5-asr-streaming-0.6b.q8_0.gguf");
        assert_eq!(language_for_model("", path), "auto");
        assert_eq!(language_for_model("auto", path), "auto");
        assert_eq!(language_for_model("da", path), "da-DK");
    }

    #[test]
    fn auto_is_pinned_to_english_for_english_checkpoint() {
        let path = Path::new("nemotron-speech-streaming-en-0.6b.q8_0.gguf");
        assert_eq!(language_for_model("", path), "en-US");
        assert_eq!(language_for_model("auto", path), "en-US");
    }

    #[test]
    fn explicit_bcp47_locale_is_preserved() {
        let path = Path::new("model.gguf");
        assert_eq!(language_for_model("fr-FR", path), "fr-FR");
        assert_eq!(language_for_model("en_US", path), "en-US");
    }

    #[test]
    fn auto_result_does_not_become_a_fake_language() {
        assert_eq!(language_result_label("auto"), None);
        assert_eq!(language_result_label("multi"), None);
        assert_eq!(language_result_label("da-DK"), Some("da".to_owned()));
    }

    #[test]
    fn auto_reports_the_primary_vulkan_accelerator_until_cpu_fallback() {
        assert_eq!(primary_accel_label("auto", "unknown"), "vulkan");
        assert_eq!(primary_accel_label("cpu", "cpu"), "cpu");
    }

    #[test]
    fn missing_model_path_is_actionable_before_loading_the_library() {
        let error = config_from_settings("missing-model.gguf", "cpu", None, None, None, false)
            .expect_err("missing model must fail before a dynamic load");
        assert!(error.to_string().contains("model file does not exist"));
    }

    #[test]
    fn config_plans_official_assets_without_bootstrapping_them() {
        let directory = tempfile::tempdir().expect("temporary library directory");
        let library = directory.path().join("nemo_speech_asr_c.dll");
        std::fs::write(&library, b"fixture").expect("write library fixture");

        let config = config_from_settings(
            "nvidia/nemotron-3.5-asr-streaming-0.6b",
            "cpu",
            None,
            None,
            Some(&library.display().to_string()),
            true,
        )
        .expect("official model should be planned without a download");

        assert!(config.local_only);
        assert!(config
            .model_path
            .ends_with("nemotron-3.5-asr-streaming-0.6b.q8_0.gguf"));
        assert_eq!(config.library_path, library);
    }

    #[test]
    fn local_prompt_and_replacements_share_the_nemotron_dictionary() {
        use crate::dictionary::DictionaryProvider;

        let directory = tempfile::tempdir().unwrap();
        let dictionary = directory.path().join("nemotron-dictionary.json");
        std::fs::write(
            &dictionary,
            r#"{"terms":["Codex"],"replacements":{"cloud code":"Claude Code"}}"#,
        )
        .unwrap();

        let backend = NemotronLocalTranscribeBackend::new(
            NemotronLocalBackendConfig {
                model_path: PathBuf::from("fixture.gguf"),
                library_path: PathBuf::from("fixture.dll"),
                gpu: -1,
                accel_label: "cpu",
                language: None,
                initial_prompt: None,
                local_only: true,
                model_request: "fixture.gguf".to_owned(),
                library_override: None,
                device: "cpu".to_owned(),
            },
            None,
        )
        .with_reloading_prompt_settings(crate::dictionary::RuntimeDictionarySettings::new(
            true,
            vec![dictionary],
            80,
            1_200,
        ));
        let prompt = backend.effective_prompt();
        let mut reload = backend
            .prompt_reload
            .as_ref()
            .expect("Nemotron backend owns prompt dictionary")
            .lock()
            .unwrap();
        let (rewritten, changes) = reload
            .current()
            .apply_replacements("open cloud code")
            .unwrap();

        assert_eq!(prompt.0.as_deref(), Some("Vocabulary: Codex"));
        assert_eq!(prompt.1, ["Codex"]);
        assert_eq!(rewritten, "open Claude Code");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].from, "cloud code");
        assert_eq!(changes[0].to, "Claude Code");
        assert_eq!(changes[0].count, 1);
    }
}
