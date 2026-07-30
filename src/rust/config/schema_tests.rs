use super::{effective_runtime_config, runtime_settings, RuntimeSetting};

#[test]
fn public_setup_metadata_api_is_populated() {
    let settings: &[RuntimeSetting] = runtime_settings();
    assert!(!settings.is_empty());
    assert!(settings
        .iter()
        .any(|setting| setting.key == "stt_backend" && !setting.choices.is_empty()));

    let effective = effective_runtime_config();
    assert!(effective.contains_key("model"));
    assert!(effective.contains_key("stt_backend"));
}
