use super::SR;

/// Resolve the live recording cap for the direct native pump. The session and
/// its tests also compile in the default feature set.
#[cfg(test)]
pub(super) fn max_record_samples_from_env() -> Option<usize> {
    max_record_samples(None)
}

pub(super) fn max_record_samples(configured_seconds: Option<f64>) -> Option<usize> {
    const DEFAULT_MAX_RECORD_S: f64 = 120.0;
    let raw = configured_seconds
        .map(|seconds| seconds.to_string())
        .or_else(|| std::env::var("VOICEPI_MAX_RECORD_S").ok());
    let trimmed = raw.as_deref().map(str::trim).unwrap_or("");
    let parsed = trimmed.parse::<f64>().ok();
    if parsed == Some(0.0) {
        return None;
    }
    let seconds = parsed
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .unwrap_or(DEFAULT_MAX_RECORD_S);
    if parsed.is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0) {
        crate::diag::log!(
            "[runtime] invalid VOICEPI_MAX_RECORD_S={trimmed:?}; using {DEFAULT_MAX_RECORD_S}s safety cap"
        );
    }
    Some(
        (seconds * f64::from(SR))
            .clamp(1.0, usize::MAX as f64)
            .round() as usize,
    )
}

pub(super) fn parse_max_record_seconds(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
}
