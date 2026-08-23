#![allow(dead_code)]

//! Utterance-boundary reload for settings marked `live` in the shared schema.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::dictate::{DictateSession, InjectBackend, TranscribeBackend};

pub(crate) const RELEASE_TAIL_ENV: &str = "VOICEPI_RELEASE_TAIL_MS";

#[derive(Default)]
pub(crate) struct LiveEnvOverrides {
    /// Caller-owned environment captured before session values are applied.
    pub(super) ambient: BTreeMap<String, String>,
    /// Explicit command-line values; these outrank config for this run.
    pub(super) forced: BTreeMap<String, String>,
    /// Caller-selected config path for `wd run --config`. `None` uses the
    /// normal application config location.
    pub(super) config_path: Option<std::path::PathBuf>,
    /// Construction-time post processor. It is restart-only, but is needed to
    /// normalize the live `post_model` value at every utterance boundary.
    pub(super) post_processor: String,
}

/// Refresh the session-owned overlay from config.json without changing the
/// process environment. Explicit CLI values remain authoritative for the run.
pub(super) fn reload<T, I>(
    session: &mut DictateSession<T, I>,
    overrides: &LiveEnvOverrides,
) -> Result<Duration, String>
where
    T: TranscribeBackend,
    I: InjectBackend,
{
    let mut settings = BTreeMap::new();
    let mut cleared = Vec::new();
    let mut forced = 0_usize;
    let resolved = match overrides.config_path.as_deref() {
        Some(path) => crate::config::load_raw_config_from_path(path)
            .map(|raw| crate::config::effective_live_runtime_settings_from_raw(&raw))
            .map_err(|err| format!("reload selected config {}: {err:#}", path.display()))?,
        None => crate::config::effective_live_runtime_settings(),
    };
    for (key, (env_name, resolved_value, configured)) in resolved {
        if overrides.forced.contains_key(&env_name) {
            forced += 1;
        }
        let value = select_live_value(&env_name, resolved_value, configured, overrides);
        match value {
            Some(value) => {
                settings.insert(key, value);
            }
            None => {
                settings.insert(key.clone(), String::new());
                cleared.push(key);
            }
        }
    }
    if let Some(model) = settings.get_mut("post_model") {
        crate::config::normalize_groq_post_model(&overrides.post_processor, model);
    }
    if let Some(level) = settings.get("log_level") {
        crate::diag::configure_level(level);
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
    let release_tail = release_tail_duration(settings.get("release_tail_ms").map(String::as_str));
    session.update_live_settings(settings);
    Ok(release_tail)
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
    use std::sync::{Arc, Mutex};

    struct RecordingTranscribe(Arc<Mutex<Vec<BTreeMap<String, String>>>>);

    impl crate::dictate::TranscribeBackend for RecordingTranscribe {
        fn transcribe(
            &self,
            _pcm: &[f32],
            _sample_rate: u32,
        ) -> Result<crate::dictate::TranscribeResult, crate::dictate::TranscribeError> {
            Ok(crate::dictate::TranscribeResult::default())
        }

        fn apply_profile_overrides(&self, settings: &BTreeMap<String, String>) {
            self.0
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(settings.clone());
        }
    }

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
            config_path: None,
            ..LiveEnvOverrides::default()
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
            config_path: None,
            ..LiveEnvOverrides::default()
        };

        assert_eq!(
            select_live_value("VOICEPI_LANG", Some("de".to_owned()), true, &overrides).as_deref(),
            Some("en")
        );
    }

    #[test]
    fn malformed_selected_config_returns_error_and_keeps_session_state() {
        let dir = tempfile::tempdir().unwrap();
        let selected = dir.path().join("selected.json");
        std::fs::write(&selected, "{ malformed").unwrap();
        let overrides = LiveEnvOverrides {
            config_path: Some(selected.clone()),
            ..LiveEnvOverrides::default()
        };
        let config = crate::dictate::SessionConfig {
            min_record_seconds: 0.9,
            ..crate::dictate::SessionConfig::default()
        };
        let mut session = DictateSession::new(
            crate::runtime::rust_session_sink::StubTranscribe,
            crate::runtime::rust_session_sink::StubInject,
            config,
        );

        let err = reload(&mut session, &overrides).expect_err("selected JSON is malformed");
        assert!(err.contains(&selected.display().to_string()));
        let mut output = Vec::new();
        session.start(&mut output).unwrap();
        session.push_frame(&vec![0.1; 9_600]);
        assert!(matches!(
            session.stop_and_transcribe(&mut output).unwrap(),
            crate::dictate::UtteranceOutcome::Skipped { .. }
        ));
    }

    #[test]
    fn selected_config_reload_uses_its_own_live_values() {
        let dir = tempfile::tempdir().unwrap();
        let selected = dir.path().join("selected.json");
        std::fs::write(
            &selected,
            serde_json::json!({
                "release_tail_ms": "17.9",
                "min_record_seconds": "0.3",
                "lang": "en"
            })
            .to_string(),
        )
        .unwrap();
        let overrides = LiveEnvOverrides {
            config_path: Some(selected),
            ..LiveEnvOverrides::default()
        };
        let mut session = DictateSession::new(
            crate::runtime::rust_session_sink::StubTranscribe,
            crate::runtime::rust_session_sink::StubInject,
            crate::dictate::SessionConfig::default(),
        );

        assert_eq!(
            reload(&mut session, &overrides).unwrap(),
            Duration::from_millis(17)
        );
    }

    #[test]
    fn recording_boundary_reload_keeps_stale_groq_model_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let selected = dir.path().join("selected.json");
        std::fs::write(
            &selected,
            serde_json::json!({"post_model": "llama-3.1-8b-instant"}).to_string(),
        )
        .unwrap();
        let overrides = LiveEnvOverrides {
            config_path: Some(selected),
            post_processor: "groq".to_owned(),
            ..LiveEnvOverrides::default()
        };
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut session = DictateSession::new(
            RecordingTranscribe(Arc::clone(&observed)),
            crate::runtime::rust_session_sink::StubInject,
            crate::dictate::SessionConfig::default(),
        );

        reload(&mut session, &overrides).unwrap();
        let mut output = Vec::new();
        session.start(&mut output).unwrap();

        let snapshots = observed.lock().unwrap_or_else(|poison| poison.into_inner());
        assert_eq!(
            snapshots.last().unwrap()["post_model"],
            crate::config::DEFAULT_GROQ_POST_MODEL
        );
    }
}
