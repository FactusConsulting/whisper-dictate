//! Validation of [`AppSettings`] before they are persisted.
//!
//! `validate` is split into grouped checks (enum choices, backend-conditional
//! URL/model requirements, numeric ranges) so each unit stays small.

use anyhow::{anyhow, Result};

use crate::config::settings::AppSettings;

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
        validate_choice("stt_provider", &self.stt_provider, &["groq", "openai"])?;
        validate_choice("device", &self.device, &["auto", "cuda", "cpu"])?;
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
            validate_http_url("stt_base_url", &self.stt_base_url)?;
            if self.stt_model.trim().is_empty() {
                return Err(anyhow!("stt_model is required when stt_backend is openai"));
            }
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

    /// Validate the numeric (integer and float) fields and their lower bounds.
    fn validate_numbers(&self) -> Result<()> {
        validate_u32("stt_timeout_ms", &self.stt_timeout_ms, 100)?;
        validate_u32("beam_size", &self.beam_size, 1)?;
        validate_u32("vad_min_silence_ms", &self.vad_min_silence_ms, 0)?;
        validate_u32("vad_speech_pad_ms", &self.vad_speech_pad_ms, 0)?;
        validate_u32("dictionary_max_terms", &self.dictionary_max_terms, 1)?;
        validate_u32("dictionary_prompt_chars", &self.dictionary_prompt_chars, 1)?;
        validate_u32("post_timeout_ms", &self.post_timeout_ms, 100)?;
        validate_u32("post_max_input_chars", &self.post_max_input_chars, 100)?;
        validate_u32("post_max_output_chars", &self.post_max_output_chars, 100)?;
        validate_u32("quit_count", &self.quit_count, 0)?;
        validate_u32("quit_window_ms", &self.quit_window_ms, 1)?;
        // Second-hotkey profile index: same treatment as every other
        // numeric-string setting so a hand-edited `config.json` with a
        // negative / non-integer value fails fast on load instead of
        // silently defaulting downstream. Codex-2 review finding on
        // #439: sibling numeric fields all get this call — the profile
        // index was the odd one out.
        validate_u32(
            "postprocess_profile_index",
            &self.postprocess_profile_index,
            0,
        )?;
        validate_f32("vad_threshold", &self.vad_threshold)?;
        validate_f32("target_dbfs", &self.target_dbfs)?;
        validate_f32("min_input_dbfs", &self.min_input_dbfs)?;
        validate_f32("min_snr_db", &self.min_snr_db)?;
        validate_f32("release_tail_ms", &self.release_tail_ms)?;
        validate_f32("preview_seconds", &self.preview_seconds)?;
        validate_f32("max_record_s", &self.max_record_s)?;
        validate_f32("context_min_seconds", &self.context_min_seconds)?;
        validate_f32("min_record_seconds", &self.min_record_seconds)?;
        validate_f32("max_chars_per_second", &self.max_chars_per_second)?;
        validate_f32("audio_ducking_level", &self.audio_ducking_level)?;
        validate_f32("ui_text_scale", &self.ui_text_scale)?;
        Ok(())
    }
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

    #[test]
    fn settings_validation_rejects_invalid_numeric_values() {
        let settings = AppSettings {
            beam_size: "fast".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("beam_size"));
    }

    /// Codex-2 review finding on #439: `postprocess_profile_index` was the
    /// only numeric-string setting that skipped `validate_u32`, so a
    /// hand-edited config with `"abc"` or a negative value silently passed
    /// validation. Guard against a regression by asserting the same
    /// error shape sibling fields produce.
    #[test]
    fn settings_validation_rejects_non_integer_postprocess_profile_index() {
        let settings = AppSettings {
            postprocess_profile_index: "abc".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("postprocess_profile_index"));
    }

    #[test]
    fn settings_validation_rejects_negative_postprocess_profile_index() {
        let settings = AppSettings {
            postprocess_profile_index: "-1".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("postprocess_profile_index"));
    }
}
