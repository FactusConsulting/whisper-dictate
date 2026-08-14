//! Regression coverage for the native utterance-boundary live-settings overlay.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    DictateSession, InjectBackend, InjectError, SessionConfig, TranscribeBackend, TranscribeError,
    TranscribeResult, UtteranceOutcome, SR,
};
use crate::dictate::feedback::{CueKind, CueSink};
use crate::dictate::profile::StaticProfileMatcher;
use crate::platform::foreground_window::FixedForegroundWindow;

use super::tests_support::{one_second_pcm, TestInject, TestTranscribe};

struct SnoopTranscribe(RefCell<Vec<BTreeMap<String, String>>>);

impl TranscribeBackend for SnoopTranscribe {
    fn transcribe(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        Ok(TranscribeResult {
            text: "ok".to_owned(),
            ..Default::default()
        })
    }

    fn apply_profile_overrides(&self, settings: &BTreeMap<String, String>) {
        self.0.borrow_mut().push(settings.clone());
    }
}

struct SnoopInject(RefCell<Vec<BTreeMap<String, String>>>);

impl InjectBackend for SnoopInject {
    fn inject(&self, _text: &str) -> Result<(), InjectError> {
        Ok(())
    }

    fn apply_profile_overrides(&self, settings: &BTreeMap<String, String>) {
        self.0.borrow_mut().push(settings.clone());
    }
}

#[test]
fn live_settings_apply_at_start_and_profile_values_win_per_key() {
    let mut session = DictateSession::new(
        SnoopTranscribe(RefCell::new(Vec::new())),
        SnoopInject(RefCell::new(Vec::new())),
        SessionConfig::default(),
    )
    .with_profile_matcher(
        Box::new(StaticProfileMatcher::new(serde_json::json!([{
            "name": "editor",
            "match": {"process": "code"},
            "settings": {"lang": "en", "inject_mode": "paste"}
        }]))),
        Box::new(FixedForegroundWindow::from_parts(None, Some("Code.exe"))),
    );
    session.update_live_settings(BTreeMap::from([
        ("lang".to_owned(), "da".to_owned()),
        ("initial_prompt".to_owned(), "live prompt".to_owned()),
        ("inject_mode".to_owned(), "type".to_owned()),
        ("min_record_seconds".to_owned(), "0.8".to_owned()),
    ]));

    let mut output = Vec::new();
    session.start(&mut output).unwrap();
    session.push_frame(&vec![0.1_f32; (SR as f64 * 0.5) as usize]);
    let outcome = session.stop_and_transcribe(&mut output).unwrap();

    assert!(matches!(outcome, UtteranceOutcome::Skipped { .. }));
    let transcribe = session.transcribe_backend().0.borrow();
    let inject = session.inject_backend().0.borrow();
    let transcribe = transcribe.last().expect("start applied settings");
    let inject = inject.last().expect("start applied settings");
    assert_eq!(
        transcribe["lang"], "en",
        "profile overrides ambient live lang"
    );
    assert_eq!(transcribe["initial_prompt"], "live prompt");
    assert_eq!(inject["inject_mode"], "paste");
}

#[test]
fn command_hook_activity_gate_closes_external_hook_after_stop() {
    let active = Arc::new(AtomicBool::new(true));
    let session = DictateSession::new(
        SnoopTranscribe(RefCell::new(Vec::new())),
        SnoopInject(RefCell::new(Vec::new())),
        SessionConfig::default(),
    )
    .with_command_hook_activity(Arc::clone(&active));

    assert!(session.command_hook_enabled());
    active.store(false, Ordering::Release);
    assert!(
        !session.command_hook_enabled(),
        "Stop must suppress hooks from an utterance already being transcribed"
    );
}

#[test]
fn stop_boundary_settings_apply_to_the_current_utterance_backends() {
    let mut session = DictateSession::new(
        SnoopTranscribe(RefCell::new(Vec::new())),
        SnoopInject(RefCell::new(Vec::new())),
        SessionConfig::default(),
    );
    session.update_live_settings(BTreeMap::from([("lang".to_owned(), "da".to_owned())]));
    let mut output = Vec::new();
    session.start(&mut output).unwrap();
    session.push_frame(&one_second_pcm());

    session.update_live_settings(BTreeMap::from([
        ("lang".to_owned(), "en".to_owned()),
        ("inject_mode".to_owned(), "paste".to_owned()),
    ]));
    session.stop_and_transcribe(&mut output).unwrap();

    assert_eq!(
        session.transcribe_backend().0.borrow().last().unwrap()["lang"],
        "en"
    );
    assert_eq!(
        session.inject_backend().0.borrow().last().unwrap()["inject_mode"],
        "paste"
    );
}

#[test]
fn owned_command_hook_settings_update_without_ambient_config_lookup() {
    let active = Arc::new(AtomicBool::new(true));
    let mut session = DictateSession::new(
        SnoopTranscribe(RefCell::new(Vec::new())),
        SnoopInject(RefCell::new(Vec::new())),
        SessionConfig::default(),
    )
    .with_owned_command_hook_activity(active);

    assert_eq!(session.command_hook_settings(), Some(("", 2_000)));
    session.update_live_settings(BTreeMap::from([
        ("command_hook".to_owned(), "selected-hook".to_owned()),
        ("command_hook_timeout_ms".to_owned(), "250.9".to_owned()),
    ]));
    assert_eq!(
        session.command_hook_settings(),
        Some(("selected-hook", 250))
    );
}

#[test]
fn stop_boundary_dictionary_switches_replacements_for_current_utterance() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.json");
    let second = dir.path().join("second.json");
    std::fs::write(
        &first,
        r#"{"terms":["OldTerm"],"replacements":{"alpha":"old"}}"#,
    )
    .unwrap();
    std::fs::write(
        &second,
        r#"{"terms":["NewTerm"],"replacements":{"alpha":"new"}}"#,
    )
    .unwrap();

    let mut session =
        DictateSession::new(
            TestTranscribe::returning_text("alpha"),
            TestInject::new(),
            SessionConfig::default(),
        )
        .with_reloading_dictionary_settings(
            crate::dictionary::RuntimeDictionarySettings::new(true, vec![first], 10, 1_200),
        );
    let mut output = Vec::new();
    session.start(&mut output).unwrap();
    session.push_frame(&one_second_pcm());
    session.update_live_settings(BTreeMap::from([(
        "dictionary".to_owned(),
        second.display().to_string(),
    )]));

    let outcome = session.stop_and_transcribe(&mut output).unwrap();
    assert!(matches!(
        outcome,
        UtteranceOutcome::Injected { ref text, .. } if text == "new"
    ));
    assert_eq!(
        session.inject_backend().injected.borrow().as_slice(),
        ["new"]
    );
}

struct SettingsAwareCue {
    enabled: AtomicBool,
    played: Arc<Mutex<Vec<(CueKind, bool)>>>,
}

impl CueSink for SettingsAwareCue {
    fn play(&self, kind: CueKind) {
        self.played
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((kind, self.enabled.load(Ordering::Relaxed)));
    }

    fn apply_settings(&self, settings: &BTreeMap<String, String>) {
        if let Some(value) = settings.get("feedback_sounds") {
            self.enabled.store(value == "1", Ordering::Relaxed);
        }
    }
}

#[test]
fn stop_boundary_feedback_setting_applies_before_the_stop_cue() {
    let played = Arc::new(Mutex::new(Vec::new()));
    let cue = SettingsAwareCue {
        enabled: AtomicBool::new(false),
        played: Arc::clone(&played),
    };
    let mut session = DictateSession::new(
        TestTranscribe::returning_text("ok"),
        TestInject::new(),
        SessionConfig::default(),
    )
    .with_cue_sink(Box::new(cue));
    session.update_live_settings(BTreeMap::from([(
        "feedback_sounds".to_owned(),
        "0".to_owned(),
    )]));
    let mut output = Vec::new();
    session.start(&mut output).unwrap();
    session.push_frame(&one_second_pcm());
    session.update_live_settings(BTreeMap::from([(
        "feedback_sounds".to_owned(),
        "1".to_owned(),
    )]));
    session.stop_and_transcribe(&mut output).unwrap();

    assert_eq!(
        *played
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        vec![(CueKind::Start, false), (CueKind::Stop, true)]
    );
}
