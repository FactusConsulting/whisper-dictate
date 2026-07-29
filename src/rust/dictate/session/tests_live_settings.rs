//! Regression coverage for the native utterance-boundary live-settings overlay.

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::{
    DictateSession, InjectBackend, InjectError, SessionConfig, TranscribeBackend, TranscribeError,
    TranscribeResult, UtteranceOutcome, SR,
};
use crate::dictate::profile::StaticProfileMatcher;
use crate::platform::foreground_window::FixedForegroundWindow;

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
    assert_eq!(
        transcribe[0]["lang"], "en",
        "profile overrides ambient live lang"
    );
    assert_eq!(transcribe[0]["initial_prompt"], "live prompt");
    assert_eq!(inject[0]["inject_mode"], "paste");
}
