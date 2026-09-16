use super::tabs::reset_tab_settings;
use super::*;

fn changed_settings() -> AppSettings {
    AppSettings {
        stt_backend: "openai".to_owned(),
        model: "large-v3".to_owned(),
        stt_provider: "groq".to_owned(),
        stt_model: "whisper-large-v3".to_owned(),
        stt_base_url: "https://api.groq.com/openai/v1".to_owned(),
        stt_timeout_ms: "12345".to_owned(),
        device: "cuda".to_owned(),
        audio_device: "Yeti".to_owned(),
        lang: "da".to_owned(),
        xkb_layout: "dk".to_owned(),
        key: "shift_r+ctrl_r".to_owned(),
        max_chars_per_second: "45".to_owned(),
        min_record_seconds: "0.8".to_owned(),
        release_tail_ms: "350".to_owned(),
        preview_seconds: "5".to_owned(),
        max_record_s: "60".to_owned(),
        target_dbfs: "-18".to_owned(),
        min_input_dbfs: "-48".to_owned(),
        min_snr_db: "10".to_owned(),
        audio_ducking: true,
        audio_ducking_level: "0.5".to_owned(),
        initial_prompt: "Keep Factus terms.".to_owned(),
        dictionary: "custom-dictionary.json".to_owned(),
        dictionary_enabled: false,
        dictionary_max_terms: "12".to_owned(),
        dictionary_prompt_chars: "345".to_owned(),
        ui_theme: "light".to_owned(),
        inject_mode: "paste".to_owned(),
        format_commands: "all".to_owned(),
        inject_json: true,
        metrics_jsonl: "metrics.jsonl".to_owned(),
        command_hook: "hook.exe".to_owned(),
        command_hook_timeout_ms: "3333".to_owned(),
        history_enabled: false,
        history_jsonl: "history.jsonl".to_owned(),
        local_only: true,
        feedback_sounds: true,
        log_level: "trace".to_owned(),
        toggle_mode: true,
        update_check: false,
        update_check_interval_minutes: "30".to_owned(),
        update_include_prereleases: true,
        ui_text_scale: "1.35".to_owned(),
        ui_log_view: "debug".to_owned(),
        ui_settings_mode: "simple".to_owned(),
        ui_autostart_runtime: true,
        post_processor: "groq".to_owned(),
        post_mode: "clean".to_owned(),
        post_model: "llama-3.3-70b-versatile".to_owned(),
        post_base_url: "https://api.groq.com/openai/v1".to_owned(),
        post_timeout_ms: "9999".to_owned(),
        post_max_input_chars: "1234".to_owned(),
        post_max_output_chars: "2345".to_owned(),
        post_redact: true,
        post_redact_terms: "Sara,Lars".to_owned(),
        ui_language: "da".to_owned(),
        profiles_json: r#"[{"name":"code"}]"#.to_owned(),
    }
}

#[test]
fn speech_page_reset_restores_only_speech_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Speech, SettingsMode::Advanced);

    assert_eq!(settings.stt_backend, defaults.stt_backend);
    assert_eq!(settings.model, defaults.model);
    assert_eq!(settings.stt_provider, defaults.stt_provider);
    assert_eq!(settings.stt_model, defaults.stt_model);
    assert_eq!(settings.stt_base_url, defaults.stt_base_url);
    assert_eq!(settings.stt_timeout_ms, defaults.stt_timeout_ms);
    assert_eq!(settings.device, defaults.device);
    assert_eq!(settings.audio_device, defaults.audio_device);
    assert_eq!(settings.lang, defaults.lang);
    assert_eq!(settings.xkb_layout, defaults.xkb_layout);
    assert_eq!(settings.key, defaults.key);
    assert_eq!(settings.toggle_mode, defaults.toggle_mode);
    assert_eq!(settings.post_processor, "groq");
}

/// Codex P1: Simple mode must only reset the settings it actually shows on
/// the Speech page — `device`, `stt_timeout_ms`, `xkb_layout`, and
/// `toggle_mode` are hidden there, so a Simple-mode user who clicks Reset
/// (seeing only engine/model/provider/key/language/mic) must not have those
/// silently wiped along with the visible fields.
///
/// `stt_base_url` is deliberately asserted as RESET here, not preserved
/// (Codex P1 follow-up, credential-routing bug): it is functionally coupled
/// to `stt_provider`, which this reset DOES touch (`changed_settings()`'s
/// provider is `"groq"`). This test previously asserted the opposite —
/// `stt_base_url` unchanged at the old Groq endpoint while `stt_provider`
/// reset to OpenAI — which pinned exactly the mismatched
/// provider/endpoint pair the fix above eliminates: a subsequent cloud API
/// check (or a real request) would have sent the newly-selected provider's
/// key to the stale provider's URL.
#[test]
fn speech_page_reset_in_simple_mode_only_touches_visible_speech_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();
    settings.ui_settings_mode = "simple".to_owned();

    reset_tab_settings(&mut settings, Tab::Speech, SettingsMode::Simple);

    // Visible in Simple: reset.
    assert_eq!(settings.stt_backend, defaults.stt_backend);
    assert_eq!(settings.model, defaults.model);
    assert_eq!(settings.stt_provider, defaults.stt_provider);
    assert_eq!(settings.stt_model, defaults.stt_model);
    assert_eq!(settings.audio_device, defaults.audio_device);
    assert_eq!(settings.lang, defaults.lang);
    assert_eq!(settings.key, defaults.key);
    // Coupled to the now-reset `stt_provider`: reset with it even though
    // the endpoint row itself is hidden in Simple mode.
    assert_eq!(settings.stt_base_url, defaults.stt_base_url);
    // Hidden in Simple AND not coupled to anything visible: NOT reset,
    // keeps its changed_settings() value.
    assert_eq!(settings.device, "cuda");
    assert_eq!(settings.stt_timeout_ms, "12345");
    assert_eq!(settings.xkb_layout, "dk");
    assert!(settings.toggle_mode);
}

/// Codex P1: the provider/endpoint pair must land back in a mutually
/// consistent state, not just each field individually matching ITS OWN
/// default in isolation — assert both together, plus that the reset
/// endpoint is genuinely OpenAI's (the default provider), not merely
/// "some non-Groq string", so a future default-provider change can't
/// silently defeat this test.
#[test]
fn speech_page_reset_in_simple_mode_resets_provider_and_endpoint_together() {
    let mut settings = changed_settings();
    settings.ui_settings_mode = "simple".to_owned();
    settings.stt_provider = "groq".to_owned();
    settings.stt_base_url = "https://api.groq.com/openai/v1".to_owned();

    reset_tab_settings(&mut settings, Tab::Speech, SettingsMode::Simple);

    assert_eq!(settings.stt_provider, AppSettings::default().stt_provider);
    assert_eq!(settings.stt_base_url, AppSettings::default().stt_base_url);
    assert_eq!(
        CloudProvider::from_raw(&settings.stt_provider),
        Some(CloudProvider::OpenAi),
        "the default provider must be OpenAI",
    );
    assert_eq!(
        settings.stt_base_url, "https://api.openai.com/v1",
        "the reset endpoint must be OpenAI's own default, not left at Groq's",
    );
}

/// Codex P1: proves the coupling fix closes the credential-routing hole
/// end to end, not just at the field-equality level — a cloud API check
/// built from the just-reset settings must target the NEW provider's own
/// endpoint. Mirrors Codex's exact repro: reset while configured for Groq;
/// reset also drops `stt_backend` back to `"whisper"` (hiding the cloud
/// pickers) and `stt_model` to empty, so re-enabling Cloud and picking a
/// model — as a user would do next — is simulated explicitly before
/// building the check. Before the fix, `check.base_url` here was still
/// Groq's, while the credential loaded for it (by `reload_stt_api_key`,
/// not exercised by this pure-settings test) would already have been the
/// newly-reset OpenAI provider's key.
#[test]
fn cloud_api_check_after_simple_reset_targets_the_reset_providers_endpoint() {
    let mut settings = changed_settings();
    settings.ui_settings_mode = "simple".to_owned();
    settings.stt_provider = "groq".to_owned();
    settings.stt_base_url = "https://api.groq.com/openai/v1".to_owned();

    reset_tab_settings(&mut settings, Tab::Speech, SettingsMode::Simple);
    settings.stt_backend = "openai".to_owned();
    settings.stt_model = "gpt-4o-mini-transcribe".to_owned();

    let check = crate::cloud_api::CloudApiCheck::from_settings(&settings, "sk-test-key").unwrap();

    assert_eq!(check.provider, "OpenAI");
    assert_eq!(check.base_url, "https://api.openai.com/v1");
}

/// The base-URL exception (Custom provider has no other way to set its
/// endpoint) applies to Reset too: a Simple-mode Custom-provider user who can
/// see the field must have Reset actually reset it.
#[test]
fn speech_page_reset_in_simple_mode_still_resets_base_url_for_custom_provider() {
    let mut settings = changed_settings();
    settings.ui_settings_mode = "simple".to_owned();
    settings.stt_provider = "custom".to_owned();

    reset_tab_settings(&mut settings, Tab::Speech, SettingsMode::Simple);

    assert_eq!(settings.stt_base_url, AppSettings::default().stt_base_url);
}

/// Codex: same ordering bug class as the Custom-provider case above, now for
/// the hosted-Nemotron-multilingual-warning exception. `stt_base_url_visible`
/// must see the model AS IT STOOD before `stt_model` gets reset a few lines
/// earlier in `reset_tab_settings` — otherwise the warning-based exception
/// evaluates against the just-cleared (empty) model instead of the real one,
/// and a visibly-shown API URL row silently fails to reset.
#[test]
fn speech_page_reset_in_simple_mode_still_resets_base_url_for_hosted_multilingual_nemotron_warning()
{
    let mut settings = changed_settings();
    settings.ui_settings_mode = "simple".to_owned();
    settings.stt_provider = "nemotron".to_owned();
    settings.stt_model = "nvidia/nemotron-asr-streaming".to_owned();
    settings.stt_base_url = "grpc://grpc.nvcf.nvidia.com:443".to_owned();

    reset_tab_settings(&mut settings, Tab::Speech, SettingsMode::Simple);

    assert_eq!(settings.stt_base_url, AppSettings::default().stt_base_url);
}

#[test]
fn quality_page_reset_restores_only_quality_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Quality, SettingsMode::Advanced);

    assert_eq!(settings.max_chars_per_second, defaults.max_chars_per_second);
    assert_eq!(settings.min_record_seconds, defaults.min_record_seconds);
    assert_eq!(settings.release_tail_ms, defaults.release_tail_ms);
    assert_eq!(settings.preview_seconds, defaults.preview_seconds);
    assert_eq!(settings.max_record_s, defaults.max_record_s);
    assert_eq!(settings.target_dbfs, defaults.target_dbfs);
    assert_eq!(settings.min_input_dbfs, defaults.min_input_dbfs);
    assert_eq!(settings.min_snr_db, defaults.min_snr_db);
    assert_eq!(settings.audio_ducking, defaults.audio_ducking);
    assert_eq!(settings.audio_ducking_level, defaults.audio_ducking_level);
    assert_eq!(settings.initial_prompt, defaults.initial_prompt);
    assert_eq!(settings.stt_backend, "openai");
    assert_eq!(settings.dictionary_max_terms, "12");
}

#[test]
fn dictionary_page_reset_restores_only_dictionary_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Dictionary, SettingsMode::Advanced);

    assert_eq!(settings.dictionary, defaults.dictionary);
    assert_eq!(settings.dictionary_enabled, defaults.dictionary_enabled);
    assert_eq!(settings.dictionary_max_terms, defaults.dictionary_max_terms);
    assert_eq!(
        settings.dictionary_prompt_chars,
        defaults.dictionary_prompt_chars
    );
    assert_eq!(settings.inject_mode, "paste");
    assert_eq!(settings.post_processor, "groq");
}

#[test]
fn output_page_reset_restores_only_output_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Output, SettingsMode::Advanced);

    // Speech-output settings that remain on the Output tab reset here.
    assert_eq!(settings.inject_mode, defaults.inject_mode);
    assert_eq!(settings.format_commands, defaults.format_commands);
    assert_eq!(settings.command_hook, defaults.command_hook);
    assert_eq!(
        settings.command_hook_timeout_ms,
        defaults.command_hook_timeout_ms
    );
    assert_eq!(settings.history_enabled, defaults.history_enabled);
    assert_eq!(settings.history_jsonl, defaults.history_jsonl);
    // App-level settings that moved to the System tab must NOT reset here.
    assert_eq!(settings.ui_theme, "light");
    assert_eq!(settings.ui_language, "da");
    assert_eq!(settings.ui_log_view, "debug");
    assert_eq!(settings.ui_text_scale, "1.35");
    assert_eq!(settings.ui_settings_mode, "simple");
    assert!(!settings.update_check);
    assert_eq!(settings.update_check_interval_minutes, "30");
    assert!(settings.update_include_prereleases);
    assert!(settings.inject_json);
    assert_eq!(settings.metrics_jsonl, "metrics.jsonl");
    assert!(settings.local_only);
    assert!(settings.feedback_sounds);
    assert_eq!(settings.log_level, "trace");
    // Unrelated pages are untouched.
    assert_eq!(settings.lang, "da");
    assert_eq!(settings.stt_backend, "openai");
}

/// Codex P1: Simple mode's Output page shows only `inject_mode` — Reset must
/// not silently wipe format commands / command hook / history, which the
/// user cannot see there.
#[test]
fn output_page_reset_in_simple_mode_only_touches_inject_mode() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();
    settings.ui_settings_mode = "simple".to_owned();

    reset_tab_settings(&mut settings, Tab::Output, SettingsMode::Simple);

    assert_eq!(settings.inject_mode, defaults.inject_mode);
    // Hidden in Simple: NOT reset.
    assert_eq!(settings.format_commands, "all");
    assert_eq!(settings.command_hook, "hook.exe");
    assert_eq!(settings.command_hook_timeout_ms, "3333");
    assert!(!settings.history_enabled);
    assert_eq!(settings.history_jsonl, "history.jsonl");
}

#[test]
fn system_page_reset_restores_only_system_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::System, SettingsMode::Advanced);

    // Appearance / display / feedback / diagnostics / integration settings reset here.
    assert_eq!(settings.ui_theme, defaults.ui_theme);
    assert_eq!(settings.ui_language, defaults.ui_language);
    assert_eq!(settings.ui_log_view, defaults.ui_log_view);
    assert_eq!(settings.ui_text_scale, defaults.ui_text_scale);
    // ui_settings_mode is the Simple/Advanced switch itself and must survive
    // a page reset unchanged (it is applied instantly by set_settings_mode,
    // never through this page-scoped Reset action).
    assert_eq!(settings.ui_settings_mode, "simple");
    assert_eq!(settings.update_check, defaults.update_check);
    assert_eq!(
        settings.update_check_interval_minutes,
        defaults.update_check_interval_minutes
    );
    assert_eq!(
        settings.update_include_prereleases,
        defaults.update_include_prereleases
    );
    assert_eq!(settings.inject_json, defaults.inject_json);
    assert_eq!(settings.metrics_jsonl, defaults.metrics_jsonl);
    assert_eq!(settings.local_only, defaults.local_only);
    assert_eq!(settings.feedback_sounds, defaults.feedback_sounds);
    assert_eq!(settings.log_level, defaults.log_level);
    // Speech-output settings that stayed on Output must NOT reset here.
    assert_eq!(settings.inject_mode, "paste");
    assert_eq!(settings.command_hook, "hook.exe");
    assert!(!settings.history_enabled);
    // Unrelated pages are untouched.
    assert_eq!(settings.stt_backend, "openai");
}

/// Codex P1: Simple mode's System page shows only theme/language — Reset
/// must not silently wipe `local_only`, Updates, Feedback, Diagnostics, or
/// Integration settings the user cannot see there.
#[test]
fn system_page_reset_in_simple_mode_only_touches_appearance() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();
    settings.ui_settings_mode = "simple".to_owned();

    reset_tab_settings(&mut settings, Tab::System, SettingsMode::Simple);

    // Visible in Simple: reset.
    assert_eq!(settings.ui_theme, defaults.ui_theme);
    assert_eq!(settings.ui_language, defaults.ui_language);
    // ui_settings_mode always survives a page reset (see the Advanced-mode
    // test above).
    assert_eq!(settings.ui_settings_mode, "simple");
    // Hidden in Simple: NOT reset, keeps its changed_settings() value.
    assert_eq!(settings.ui_log_view, "debug");
    assert_eq!(settings.ui_text_scale, "1.35");
    assert!(!settings.update_check);
    assert_eq!(settings.update_check_interval_minutes, "30");
    assert!(settings.update_include_prereleases);
    assert!(settings.inject_json);
    assert_eq!(settings.metrics_jsonl, "metrics.jsonl");
    assert!(settings.local_only);
    assert!(settings.feedback_sounds);
    assert_eq!(settings.log_level, "trace");
}

#[test]
fn post_page_reset_restores_only_post_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Post, SettingsMode::Advanced);

    assert_eq!(settings.post_processor, defaults.post_processor);
    assert_eq!(settings.post_mode, defaults.post_mode);
    assert_eq!(settings.post_model, defaults.post_model);
    assert_eq!(settings.post_base_url, defaults.post_base_url);
    assert_eq!(settings.post_timeout_ms, defaults.post_timeout_ms);
    assert_eq!(settings.post_max_input_chars, defaults.post_max_input_chars);
    assert_eq!(
        settings.post_max_output_chars,
        defaults.post_max_output_chars
    );
    assert_eq!(settings.post_redact, defaults.post_redact);
    assert_eq!(settings.post_redact_terms, defaults.post_redact_terms);
    assert_eq!(settings.stt_backend, "openai");
    assert_eq!(settings.dictionary_max_terms, "12");
}

#[test]
fn profiles_page_reset_restores_only_profiles_json() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Profiles, SettingsMode::Advanced);

    assert_eq!(settings.profiles_json, defaults.profiles_json);
    assert_eq!(settings.stt_backend, "openai");
    assert_eq!(settings.post_processor, "groq");
}
