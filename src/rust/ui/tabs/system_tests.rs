#![cfg(windows)]

use super::super::test_support::test_app;
use super::*;

#[test]
fn windows_default_metrics_action_clears_stale_null_intent() {
    let settings = AppSettings {
        metrics_jsonl: String::new(),
        ..Default::default()
    };
    let mut app = test_app(settings);
    app.config_path = r"C:\Users\test\AppData\Roaming\WhisperDictate\config.json".to_owned();
    app.record_nullable_selection("metrics_jsonl", "");

    app.use_default_metrics_jsonl_path();

    assert!(!app.settings.metrics_jsonl.is_empty());
    assert!(!app.explicit_nullable_clears.contains("metrics_jsonl"));
}
