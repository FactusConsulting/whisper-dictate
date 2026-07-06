use super::tabs::{reset_tab_settings, reset_tab_settings_for_mode};
use super::SettingsMode;
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
        compute_type: "int8_float16".to_owned(),
        audio_device: "Yeti".to_owned(),
        lang: "da".to_owned(),
        xkb_layout: "dk".to_owned(),
        key: "shift_r+ctrl_r".to_owned(),
        quit_key: "f12".to_owned(),
        quit_count: "4".to_owned(),
        quit_window_ms: "2000".to_owned(),
        beam_size: "5".to_owned(),
        temperature: "0.4".to_owned(),
        context_min_seconds: "1.5".to_owned(),
        hallucination_guard: false,
        max_chars_per_second: "45".to_owned(),
        min_record_seconds: "0.8".to_owned(),
        release_tail_ms: "350".to_owned(),
        preview_seconds: "5".to_owned(),
        max_record_s: "60".to_owned(),
        vad_threshold: "0.42".to_owned(),
        vad_min_silence_ms: "900".to_owned(),
        vad_speech_pad_ms: "450".to_owned(),
        target_dbfs: "-18".to_owned(),
        min_input_dbfs: "-48".to_owned(),
        min_snr_db: "10".to_owned(),
        audio_ducking: true,
        audio_ducking_level: "0.5".to_owned(),
        mute_output_while_recording: true,
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
        feedback_notify: true,
        debug: true,
        stt_debug: true,
        trace: true,
        toggle_mode: true,
        update_check: false,
        update_check_interval_minutes: "30".to_owned(),
        update_include_prereleases: true,
        ui_text_scale: "1.35".to_owned(),
        ui_log_view: "debug".to_owned(),
        post_processor: "groq".to_owned(),
        post_mode: "clean".to_owned(),
        post_model: "llama-3.3-70b-versatile".to_owned(),
        post_base_url: "https://api.groq.com/openai/v1".to_owned(),
        post_timeout_ms: "9999".to_owned(),
        post_max_input_chars: "1234".to_owned(),
        post_max_output_chars: "2345".to_owned(),
        post_redact: true,
        post_redact_terms: "Sara,Lars".to_owned(),
        postprocess_hotkey: "shift_r+ctrl_r".to_owned(),
        postprocess_profiles: r#"[{"name":"grammar"}]"#.to_owned(),
        postprocess_profile_index: "1".to_owned(),
        ui_language: "da".to_owned(),
        overlay_enabled: true,
        overlay_position: "top-left".to_owned(),
        overlay_show_on_idle: true,
        settings_mode: "advanced".to_owned(),
        profiles_json: r#"[{"name":"code"}]"#.to_owned(),
        // Issue #328: onboarding-wizard gate. The reset flow is orthogonal to
        // the wizard so it can carry any value here; the changed_settings()
        // fixture just needs a non-default sentinel to keep this legal.
        onboarding_completed: true,
        onboarding_seen_at: "2026-07-04T12:34:56Z".to_owned(),
    }
}

#[test]
fn speech_page_reset_restores_only_speech_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Speech);

    assert_eq!(settings.stt_backend, defaults.stt_backend);
    assert_eq!(settings.model, defaults.model);
    assert_eq!(settings.stt_provider, defaults.stt_provider);
    assert_eq!(settings.stt_model, defaults.stt_model);
    assert_eq!(settings.stt_base_url, defaults.stt_base_url);
    assert_eq!(settings.stt_timeout_ms, defaults.stt_timeout_ms);
    assert_eq!(settings.device, defaults.device);
    assert_eq!(settings.compute_type, defaults.compute_type);
    assert_eq!(settings.audio_device, defaults.audio_device);
    assert_eq!(settings.lang, defaults.lang);
    assert_eq!(settings.xkb_layout, defaults.xkb_layout);
    assert_eq!(settings.key, defaults.key);
    assert_eq!(settings.quit_key, defaults.quit_key);
    assert_eq!(settings.quit_count, defaults.quit_count);
    assert_eq!(settings.quit_window_ms, defaults.quit_window_ms);
    assert_eq!(settings.toggle_mode, defaults.toggle_mode);
    assert_eq!(settings.vad_threshold, "0.42");
    assert_eq!(settings.post_processor, "groq");
}

#[test]
fn quality_page_reset_restores_only_quality_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Quality);

    assert_eq!(settings.beam_size, defaults.beam_size);
    assert_eq!(settings.temperature, defaults.temperature);
    assert_eq!(settings.context_min_seconds, defaults.context_min_seconds);
    assert_eq!(settings.max_chars_per_second, defaults.max_chars_per_second);
    assert_eq!(settings.min_record_seconds, defaults.min_record_seconds);
    assert_eq!(settings.release_tail_ms, defaults.release_tail_ms);
    assert_eq!(settings.preview_seconds, defaults.preview_seconds);
    assert_eq!(settings.max_record_s, defaults.max_record_s);
    assert_eq!(settings.vad_threshold, defaults.vad_threshold);
    assert_eq!(settings.vad_min_silence_ms, defaults.vad_min_silence_ms);
    assert_eq!(settings.vad_speech_pad_ms, defaults.vad_speech_pad_ms);
    assert_eq!(settings.target_dbfs, defaults.target_dbfs);
    assert_eq!(settings.min_input_dbfs, defaults.min_input_dbfs);
    assert_eq!(settings.min_snr_db, defaults.min_snr_db);
    assert_eq!(settings.audio_ducking, defaults.audio_ducking);
    assert_eq!(settings.audio_ducking_level, defaults.audio_ducking_level);
    // Codex P2 (settings_schema.json:321, PR #440) — the new
    // mute-output toggle must reset with its sibling audio controls.
    assert_eq!(
        settings.mute_output_while_recording,
        defaults.mute_output_while_recording
    );
    assert_eq!(settings.initial_prompt, defaults.initial_prompt);
    assert_eq!(settings.stt_backend, "openai");
    assert_eq!(settings.dictionary_max_terms, "12");
}

#[test]
fn dictionary_page_reset_restores_only_dictionary_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Dictionary);

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

    reset_tab_settings(&mut settings, Tab::Output);

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
    assert!(!settings.update_check);
    assert_eq!(settings.update_check_interval_minutes, "30");
    assert!(settings.update_include_prereleases);
    assert!(settings.inject_json);
    assert_eq!(settings.metrics_jsonl, "metrics.jsonl");
    assert!(settings.local_only);
    assert!(settings.feedback_sounds);
    assert!(settings.feedback_notify);
    assert!(settings.debug);
    assert!(settings.stt_debug);
    assert!(settings.trace);
    // Unrelated pages are untouched.
    assert_eq!(settings.lang, "da");
    assert_eq!(settings.stt_backend, "openai");
    assert_eq!(settings.vad_threshold, "0.42");
}

#[test]
fn system_page_reset_restores_only_system_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::System);

    // Appearance / display / feedback / diagnostics / integration settings reset here.
    assert_eq!(settings.ui_theme, defaults.ui_theme);
    assert_eq!(settings.ui_language, defaults.ui_language);
    assert_eq!(settings.ui_log_view, defaults.ui_log_view);
    assert_eq!(settings.ui_text_scale, defaults.ui_text_scale);
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
    assert_eq!(settings.feedback_notify, defaults.feedback_notify);
    assert_eq!(settings.debug, defaults.debug);
    assert_eq!(settings.stt_debug, defaults.stt_debug);
    assert_eq!(settings.trace, defaults.trace);
    // Speech-output settings that stayed on Output must NOT reset here.
    assert_eq!(settings.inject_mode, "paste");
    assert_eq!(settings.command_hook, "hook.exe");
    assert!(!settings.history_enabled);
    // Unrelated pages are untouched.
    assert_eq!(settings.stt_backend, "openai");
    assert_eq!(settings.vad_threshold, "0.42");
}

#[test]
fn post_page_reset_restores_only_post_settings() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Post);

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
    assert_eq!(settings.postprocess_hotkey, defaults.postprocess_hotkey);
    assert_eq!(settings.postprocess_profiles, defaults.postprocess_profiles);
    assert_eq!(
        settings.postprocess_profile_index,
        defaults.postprocess_profile_index
    );
    assert_eq!(settings.stt_backend, "openai");
    assert_eq!(settings.dictionary_max_terms, "12");
}

#[test]
fn profiles_page_reset_restores_only_profiles_json() {
    let defaults = AppSettings::default();
    let mut settings = changed_settings();

    reset_tab_settings(&mut settings, Tab::Profiles);

    assert_eq!(settings.profiles_json, defaults.profiles_json);
    assert_eq!(settings.stt_backend, "openai");
    assert_eq!(settings.post_processor, "groq");
}

#[test]
fn simple_mode_speech_reset_preserves_advanced_only_fields() {
    // Issue #334 / Codex #435 P2: a Simple-mode user who resets the
    // Speech page must not lose Advanced-only knobs they configured
    // before switching modes. The reset must only touch fields Simple
    // mode actually shows (mic/hotkey/backend+model/language/cloud
    // provider+model+URL); the Advanced-only knobs (device, compute
    // type, cloud timeout, xkb layout, toggle mode, quit chord) must
    // stay as they were.
    let defaults = AppSettings::default();
    let mut settings = changed_settings();
    settings.settings_mode = "simple".to_owned();

    reset_tab_settings_for_mode(&mut settings, Tab::Speech, SettingsMode::Simple);

    // Visible-in-Simple rows reset to defaults.
    assert_eq!(settings.stt_backend, defaults.stt_backend);
    assert_eq!(settings.model, defaults.model);
    assert_eq!(settings.stt_provider, defaults.stt_provider);
    assert_eq!(settings.stt_model, defaults.stt_model);
    assert_eq!(settings.stt_base_url, defaults.stt_base_url);
    assert_eq!(settings.audio_device, defaults.audio_device);
    assert_eq!(settings.lang, defaults.lang);
    assert_eq!(settings.key, defaults.key);

    // Advanced-only Speech fields are PRESERVED, not silently reset to
    // the schema default.
    assert_eq!(settings.stt_timeout_ms, "12345");
    assert_eq!(settings.device, "cuda");
    assert_eq!(settings.compute_type, "int8_float16");
    assert_eq!(settings.xkb_layout, "dk");
    assert!(settings.toggle_mode);
    assert_eq!(settings.quit_key, "f12");
    assert_eq!(settings.quit_count, "4");
    assert_eq!(settings.quit_window_ms, "2000");
}

#[test]
fn advanced_mode_reset_dispatch_matches_full_reset() {
    // Advanced mode goes through the same reset code path a pre-#334
    // user got, so the dispatch is a strict pass-through to
    // `reset_tab_settings`. This test pins that equivalence so a future
    // change to the dispatch does not silently drift the two apart.
    let mut via_dispatch = changed_settings();
    let mut via_full = changed_settings();

    reset_tab_settings_for_mode(&mut via_dispatch, Tab::Speech, SettingsMode::Advanced);
    reset_tab_settings(&mut via_full, Tab::Speech);

    assert_eq!(via_dispatch, via_full);
}

#[test]
fn simple_mode_reset_is_noop_on_hidden_tabs() {
    // Every tab except Log + Speech is hidden from the sidebar in Simple
    // mode, so the Reset Page button is unreachable. As a defensive
    // guard, the Simple-mode reset dispatch is a no-op for those tabs —
    // if a future rendering bug ever routes a Reset Page click through
    // this path anyway, we do not want it to wipe Advanced-only fields
    // the user cannot see.
    let baseline = changed_settings();
    for tab in [
        Tab::Quality,
        Tab::Dictionary,
        Tab::Output,
        Tab::Post,
        Tab::Profiles,
        Tab::System,
    ] {
        let mut settings = baseline.clone();
        reset_tab_settings_for_mode(&mut settings, tab, SettingsMode::Simple);
        assert_eq!(
            settings, baseline,
            "Simple-mode reset for hidden tab {tab:?} must be a no-op",
        );
    }
}
