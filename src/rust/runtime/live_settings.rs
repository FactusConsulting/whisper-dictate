#![allow(dead_code)]

//! Utterance-boundary reload for settings marked `live` in the shared schema.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::dictate::{DictateSession, InjectBackend, TranscribeBackend};

pub(crate) const RELEASE_TAIL_ENV: &str = "VOICEPI_RELEASE_TAIL_MS";

/// Refresh the process environment and session-owned overlay from config.json.
/// Explicit CLI values remain authoritative for the duration of the run.
pub(crate) fn reload<T, I>(
    session: &mut DictateSession<T, I>,
    forced_env: &BTreeMap<String, String>,
) where
    T: TranscribeBackend,
    I: InjectBackend,
{
    let mut settings = BTreeMap::new();
    for (key, (env_name, resolved_value)) in crate::config::effective_live_runtime_settings() {
        let value = forced_env.get(&env_name).cloned().unwrap_or(resolved_value);
        std::env::set_var(env_name, &value);
        settings.insert(key, value);
    }
    session.update_live_settings(settings);
}

/// Parse the release-tail delay with Python-compatible float input and a
/// non-negative floor. Invalid values fall back to the schema default (200 ms).
pub(crate) fn release_tail_duration(raw: Option<&str>) -> Duration {
    let millis = raw
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
        .unwrap_or(200.0);
    Duration::from_millis(millis as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_tail_accepts_float_clamps_negative_and_defaults_invalid() {
        assert_eq!(
            release_tail_duration(Some("250.9")),
            Duration::from_millis(250)
        );
        assert_eq!(release_tail_duration(Some("-50")), Duration::ZERO);
        assert_eq!(
            release_tail_duration(Some("bad")),
            Duration::from_millis(200)
        );
        assert_eq!(release_tail_duration(None), Duration::from_millis(200));
    }
}
