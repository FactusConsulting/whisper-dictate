//! Validation of [`AppSettings`] before they are persisted.
//!
//! `validate` is split into grouped checks (enum choices, backend-conditional
//! URL/model requirements, numeric ranges) so each unit stays small.

use anyhow::{anyhow, Result};

use crate::config::settings::AppSettings;
use crate::whisper::device_options::{
    available_device_values, is_device_supported, missing_device_hint,
};

impl AppSettings {
    /// Validate every settings field, returning the first violation as an error.
    pub fn validate(&self) -> Result<()> {
        self.validate_choices()?;
        self.validate_backend_requirements()?;
        self.validate_numbers()?;
        Ok(())
    }

    /// Reject values outside the allowed set for each enum-like field.
    fn validate_choices(&self) -> Result<()> {
        validate_choice("stt_backend", &self.stt_backend, &["whisper", "openai"])?;
        validate_choice(
            "stt_provider",
            &self.stt_provider,
            &["groq", "openai", "custom", "nemotron"],
        )?;
        // `device` is enum-checked against the *build-filtered* option set so
        // `wd config set device vulkan` on a CPU-only binary fails
        // loudly instead of silently accepting a value that Whisper will just
        // demote to CPU at runtime (rc.9 Windows regression). Any dropped
        // value gets a targeted hint pointing at the rebuild flag; the enum-
        // choice validator itself handles typos / unknown values.
        if self.stt_backend == "openai" {
            // Cloud transcription does not consume the local accelerator
            // setting. Accept the legacy CUDA alias so switching to the
            // cloud backend on a CPU-only build does not make unrelated
            // Settings saves fail.
            validate_choice("device", &self.device, &["auto", "vulkan", "cuda", "cpu"])?;
        } else {
            validate_device(&self.device)?;
        }
        validate_choice(
            "inject_mode",
            &self.inject_mode,
            &["auto", "type", "paste", "print"],
        )?;
        validate_choice(
            "post_processor",
            &self.post_processor,
            &["none", "ollama", "openai", "groq"],
        )?;
        validate_choice(
            "post_mode",
            &self.post_mode,
            &[
                "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
            ],
        )?;
        validate_choice("ui_theme", &self.ui_theme, &["dark", "light"])?;
        validate_choice("ui_language", &self.ui_language, &["en", "da"])?;
        validate_choice(
            "ui_log_view",
            &self.ui_log_view,
            &["minimal", "diagnostic", "debug"],
        )?;
        Ok(())
    }

    /// Enforce the URL/model fields required when a cloud backend or an active
    /// post-processor is selected.
    fn validate_backend_requirements(&self) -> Result<()> {
        if self.stt_backend == "openai" {
            // NVIDIA's hosted Nemotron endpoint is Riva gRPC and the vendor's
            // quick-start documents it as a bare `grpc.nvcf.nvidia.com:443`
            // authority. Keep the normal HTTP URL guard for every other
            // provider, but allow that provider-scoped gRPC spelling so the
            // Speech-tab Test API can normalize it to TLS.
            let bare_nemotron_grpc = self.stt_provider.trim().eq_ignore_ascii_case("nemotron")
                && crate::cloud_api::is_nemotron_grpc_endpoint(
                    "nemotron 3.5 asr",
                    self.stt_base_url.trim(),
                );
            if !bare_nemotron_grpc {
                validate_http_url("stt_base_url", &self.stt_base_url)?;
            }
            if self.stt_model.trim().is_empty() {
                return Err(anyhow!("stt_model is required when stt_backend is openai"));
            }
            self.validate_nemotron_profile_language()?;
        }
        if matches!(self.post_processor.as_str(), "ollama" | "openai" | "groq") {
            validate_http_url("post_base_url", &self.post_base_url)?;
            if self.post_model.trim().is_empty() {
                return Err(anyhow!(
                    "post_model is required when post_processor is active"
                ));
            }
        }
        Ok(())
    }

    /// The English-only Nemotron deployment cannot perform language
    /// identification and rejects non-English locale hints. Keep this guard
    /// at the settings boundary so the UI, CLI, and persisted config all fail
    /// with the same actionable message; the backend still has a legacy Auto
    /// fallback for snapshots created before this validation existed.
    pub(crate) fn validate_nemotron_profile_language(&self) -> Result<()> {
        let provider_is_nemotron = self.stt_provider.trim().eq_ignore_ascii_case("nemotron");
        // The selected provider is authoritative. A custom OpenAI-compatible
        // endpoint may intentionally expose a Nemotron-named model, but it
        // does not necessarily implement Nemotron's English-only profile
        // contract (and must stay on the generic HTTP path).
        if provider_is_nemotron
            && crate::dictate::backends::cloud_transcribe::
                nemotron_english_profile_requires_language(&self.stt_model, &self.lang)
        {
            return Err(anyhow!(
                "Nemotron English profile requires Language=English (en); choose English or switch to the Multilingual / Auto profile"
            ));
        }
        Ok(())
    }

    /// Validate the numeric (integer and float) fields and their lower bounds.
    fn validate_numbers(&self) -> Result<()> {
        validate_u32("stt_timeout_ms", &self.stt_timeout_ms, 100)?;
        validate_u32("dictionary_max_terms", &self.dictionary_max_terms, 1)?;
        validate_u32("dictionary_prompt_chars", &self.dictionary_prompt_chars, 1)?;
        validate_u32("post_timeout_ms", &self.post_timeout_ms, 100)?;
        validate_u32("post_max_input_chars", &self.post_max_input_chars, 100)?;
        validate_u32("post_max_output_chars", &self.post_max_output_chars, 100)?;
        validate_f32("target_dbfs", &self.target_dbfs)?;
        validate_f32("min_input_dbfs", &self.min_input_dbfs)?;
        validate_f32("min_snr_db", &self.min_snr_db)?;
        validate_f32("release_tail_ms", &self.release_tail_ms)?;
        validate_f32("preview_seconds", &self.preview_seconds)?;
        validate_f32("max_record_s", &self.max_record_s)?;
        validate_f32("min_record_seconds", &self.min_record_seconds)?;
        validate_f32("max_chars_per_second", &self.max_chars_per_second)?;
        validate_f32("audio_ducking_level", &self.audio_ducking_level)?;
        validate_f32("ui_text_scale", &self.ui_text_scale)?;
        Ok(())
    }
}

/// Validate `device` against the build-filtered option list.
///
/// Uses [`crate::whisper::device_options`] so the CLI setter and the UI
/// dropdown share a single source of truth. When the value is a *legal*
/// device name that this binary can't honour (e.g. `vulkan` on a CPU-only
/// build), the error appends the rebuild / installer hint from
/// [`missing_device_hint`] so scripting users don't have to grep for it.
fn validate_device(value: &str) -> Result<()> {
    if is_device_supported(value) {
        return Ok(());
    }
    let allowed = available_device_values();
    let hint = missing_device_hint(value)
        .map(|h| format!(" - {h}"))
        .unwrap_or_default();
    Err(anyhow!(
        "device must be one of {}; got {value:?}{hint}",
        allowed.join(", "),
    ))
}

fn validate_choice(name: &str, value: &str, allowed: &[&str]) -> Result<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(anyhow!(
            "{name} must be one of {}; got {value:?}",
            allowed.join(", ")
        ))
    }
}

fn validate_http_url(name: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(anyhow!("{name} must start with http:// or https://"))
    }
}

fn validate_u32(name: &str, value: &str, minimum: u32) -> Result<()> {
    let parsed = value
        .trim()
        .parse::<u32>()
        .map_err(|_| anyhow!("{name} must be an integer"))?;
    if parsed >= minimum {
        Ok(())
    } else {
        Err(anyhow!("{name} must be at least {minimum}"))
    }
}

fn validate_f32(name: &str, value: &str) -> Result<()> {
    value
        .trim()
        .parse::<f32>()
        .map(|_| ())
        .map_err(|_| anyhow!("{name} must be a number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_validation_rejects_invalid_backend() {
        let settings = AppSettings {
            stt_backend: "cloud".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("stt_backend"));
    }

    #[test]
    fn settings_validation_rejects_invalid_ui_theme() {
        let settings = AppSettings {
            ui_theme: "solarized".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("ui_theme"));
    }

    #[test]
    fn settings_validation_rejects_invalid_ui_language() {
        let settings = AppSettings {
            ui_language: "dk".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("ui_language"));
    }

    #[test]
    fn settings_validation_rejects_invalid_ui_log_view() {
        let settings = AppSettings {
            ui_log_view: "full".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("ui_log_view"));
    }

    #[test]
    fn settings_validation_rejects_cloud_without_http_url() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_model: "whisper-large-v3-turbo".to_owned(),
            stt_base_url: "api.groq.com/openai/v1".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("stt_base_url"));
    }

    #[cfg(feature = "whisper-rs-vulkan")]
    #[test]
    fn settings_validation_accepts_cuda_on_gpu_builds() {
        let settings = AppSettings {
            device: "cuda".to_owned(),
            ..AppSettings::default()
        };
        settings.validate().unwrap();
    }

    #[cfg(not(feature = "whisper-rs-vulkan"))]
    #[test]
    fn settings_validation_rejects_cuda_on_cpu_only_builds() {
        let settings = AppSettings {
            device: "cuda".to_owned(),
            ..AppSettings::default()
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("unavailable"));
        assert!(!error.contains("Python"));
    }

    #[test]
    fn settings_validation_accepts_auto_and_cpu_on_every_build() {
        for value in ["auto", "cpu"] {
            let settings = AppSettings {
                device: value.to_owned(),
                ..AppSettings::default()
            };
            settings
                .validate()
                .unwrap_or_else(|e| panic!("device={value:?} unexpectedly rejected: {e}"));
        }
    }

    #[test]
    fn cloud_settings_accept_ignored_cuda_hint_on_every_build() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_base_url: "https://api.openai.com/v1".to_owned(),
            stt_model: "whisper-1".to_owned(),
            device: "cuda".to_owned(),
            ..AppSettings::default()
        };
        settings.validate().unwrap();
    }

    #[test]
    fn cloud_settings_accept_custom_openai_compatible_provider() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "custom".to_owned(),
            stt_base_url: "http://localhost:8000/v1".to_owned(),
            stt_model: "my-transcription-model".to_owned(),
            ..AppSettings::default()
        };
        settings.validate().unwrap();
    }

    #[test]
    fn cloud_settings_accept_nemotron_nim_provider() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "nemotron".to_owned(),
            stt_base_url: "http://localhost:9000/v1".to_owned(),
            stt_model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
            ..AppSettings::default()
        };
        settings.validate().unwrap();
    }

    #[test]
    fn cloud_settings_reject_english_nemotron_profile_with_auto_language() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "nemotron".to_owned(),
            stt_base_url: "http://localhost:9000/v1".to_owned(),
            stt_model: "nvidia/nemotron-speech-streaming-en-0.6b".to_owned(),
            lang: String::new(),
            ..AppSettings::default()
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("requires Language=English"));
    }

    #[test]
    fn cloud_settings_accept_english_nemotron_profile_with_english_language() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "nemotron".to_owned(),
            stt_base_url: "http://localhost:9000/v1".to_owned(),
            stt_model: "nvidia/nemotron-speech-streaming-en-0.6b".to_owned(),
            lang: "en".to_owned(),
            ..AppSettings::default()
        };
        settings.validate().unwrap();
    }

    #[test]
    fn cloud_settings_custom_provider_can_use_nemotron_named_model() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "custom".to_owned(),
            stt_base_url: "http://localhost:9000/v1".to_owned(),
            stt_model: "nvidia/nemotron-speech-streaming-en-0.6b".to_owned(),
            lang: String::new(),
            ..AppSettings::default()
        };
        settings.validate().unwrap();
    }

    #[test]
    fn cloud_settings_accept_nemotron_bare_hosted_grpc_endpoint() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "nemotron".to_owned(),
            stt_base_url: "grpc.nvcf.nvidia.com:443".to_owned(),
            stt_model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_owned(),
            ..AppSettings::default()
        };
        settings.validate().unwrap();
    }

    #[test]
    fn cloud_settings_still_reject_bare_url_for_other_providers() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_provider: "custom".to_owned(),
            stt_base_url: "example.test:443".to_owned(),
            stt_model: "transcriber".to_owned(),
            ..AppSettings::default()
        };
        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must start with http:// or https://"));
    }
}
