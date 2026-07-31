#![allow(dead_code)]

//! Utterance-boundary reload for settings marked `live` in the shared schema.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::dictate::{DictateSession, InjectBackend, TranscribeBackend};

pub(crate) const RELEASE_TAIL_ENV: &str = "VOICEPI_RELEASE_TAIL_MS";

#[derive(Default)]
pub(super) struct LiveEnvOverrides {
    /// Caller-owned environment captured before session values are applied.
    pub(super) ambient: BTreeMap<String, String>,
    /// Explicit command-line values; these outrank config for this run.
    pub(super) forced: BTreeMap<String, String>,
}

/// Refresh the process environment and session-owned overlay from config.json.
/// Explicit CLI values remain authoritative for the duration of the run.
pub(super) fn reload<T, I>(session: &mut DictateSession<T, I>, overrides: &LiveEnvOverrides)
where
    T: TranscribeBackend,
    I: InjectBackend,
{
    let mut settings = BTreeMap::new();
    let mut cleared = Vec::new();
    let mut forced = 0_usize;
    for (key, (env_name, resolved_value, configured)) in
        crate::config::effective_live_runtime_settings()
    {
        if overrides.forced.contains_key(&env_name) {
            forced += 1;
        }
        let value = select_live_value(&env_name, resolved_value, configured, overrides);
        match value {
            Some(value) => {
                std::env::set_var(&env_name, &value);
                settings.insert(key, value);
            }
            None => {
                std::env::remove_var(&env_name);
                settings.insert(key.clone(), String::new());
                cleared.push(key);
            }
        }
    }
    if crate::diag::debug_enabled() {
        crate::diag::log!(
            "[runtime/debug] live settings reload applied={} cleared={} forced={forced}",
            settings.len().saturating_sub(cleared.len()),
            cleared.len()
        );
    }
    if crate::diag::trace_enabled() && !cleared.is_empty() {
        crate::diag::log!("[runtime/trace] live settings cleared keys={:?}", cleared);
    }
    session.update_live_settings(settings);
    crate::diag::init_from_env();
}

fn select_live_value(
    env_name: &str,
    resolved_value: Option<String>,
    configured: bool,
    overrides: &LiveEnvOverrides,
) -> Option<String> {
    overrides.forced.get(env_name).cloned().or_else(|| {
        if configured {
            resolved_value
        } else {
            overrides.ambient.get(env_name).cloned().or(resolved_value)
        }
    })
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

    #[test]
    fn ambient_live_value_survives_when_config_key_is_missing_but_not_cleared() {
        let overrides = LiveEnvOverrides {
            ambient: BTreeMap::from([("VOICEPI_LANG".to_owned(), "da".to_owned())]),
            forced: BTreeMap::new(),
        };

        assert_eq!(
            select_live_value("VOICEPI_LANG", None, false, &overrides).as_deref(),
            Some("da")
        );
        assert_eq!(
            select_live_value("VOICEPI_LANG", None, true, &overrides),
            None,
            "an explicit empty/null config value must clear the ambient override"
        );
    }

    #[test]
    fn explicit_cli_live_value_outranks_config_and_ambient() {
        let overrides = LiveEnvOverrides {
            ambient: BTreeMap::from([("VOICEPI_LANG".to_owned(), "da".to_owned())]),
            forced: BTreeMap::from([("VOICEPI_LANG".to_owned(), "en".to_owned())]),
        };

        assert_eq!(
            select_live_value("VOICEPI_LANG", Some("de".to_owned()), true, &overrides).as_deref(),
            Some("en")
        );
    }
}
