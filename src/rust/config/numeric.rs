//! Shared numeric validation and tolerant runtime fallback.

use anyhow::{anyhow, Result};

use super::schema::{numeric_bounds, RUNTIME_SETTINGS};

fn integer_setting(key: &str) -> bool {
    matches!(
        key,
        "stt_timeout_ms"
            | "history_max_entries"
            | "dictionary_max_terms"
            | "dictionary_prompt_chars"
            | "post_timeout_ms"
            | "post_max_input_chars"
            | "post_max_output_chars"
            | "update_check_interval_minutes"
    )
}

fn float_setting(key: &str) -> bool {
    matches!(
        key,
        "target_dbfs" | "min_input_dbfs" | "min_snr_db" | "ui_text_scale"
    ) || numeric_bounds(key).is_some()
}

pub(super) fn validate_numeric(key: &str, raw: &str) -> Result<()> {
    if !float_setting(key) {
        return Ok(());
    }
    let value = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("{key} must be a finite number"))?;
    if !value.is_finite() || !(value as f32).is_finite() {
        return Err(anyhow!("{key} must be a finite number"));
    }
    if integer_setting(key) && raw.trim().parse::<u64>().is_err() {
        return Err(anyhow!("{key} must be an integer"));
    }
    if let Some(bounds) = numeric_bounds(key) {
        if value < bounds.min || value > bounds.max {
            return Err(anyhow!(
                "{key} must be between {} and {}",
                bounds.min,
                bounds.max
            ));
        }
    }
    Ok(())
}

/// Hand-edited config, environment and profile values use the schema default
/// when invalid. This does not change the source file or the process env.
/// Explicit CLI/UI saves instead call the strict validator before writing.
pub(crate) fn runtime_value(key: &str, value: String) -> String {
    if validate_numeric(key, &value).is_ok() {
        return value;
    }
    RUNTIME_SETTINGS
        .iter()
        .find(|setting| setting.key == key)
        .and_then(|setting| setting.default.clone())
        .unwrap_or_else(|| {
            debug_assert_eq!(key, "ui_text_scale");
            "1.15".to_owned()
        })
}

#[cfg(test)]
#[path = "numeric_tests.rs"]
mod tests;
