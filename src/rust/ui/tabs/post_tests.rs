use super::super::render_test_support::{contains_text, rendered_texts_for_tab};
use super::super::test_support::test_app;
use super::super::{
    AppSettings, PostProvider, SettingsMode, Tab, WhisperDictateApp, GROQ_STT_BASE_URL,
    OPENAI_STT_BASE_URL,
};
use super::post::GROQ_POST_MODEL_HELP;

#[test]
fn groq_post_model_help_describes_only_current_picker_choices() {
    assert!(GROQ_POST_MODEL_HELP.contains("recommended fast cleanup"));
    assert!(GROQ_POST_MODEL_HELP.contains("heavier highest-quality"));
    for retired_category in ["Danish", "reasoning", "preview"] {
        assert!(
            !GROQ_POST_MODEL_HELP.contains(retired_category),
            "help still advertises removed category {retired_category:?}"
        );
    }
}

/// Headless-rendering proof that `post_processing_tab` applies the
/// Simple/Advanced gating itself — see `render_test_support`.
fn app_in_mode(mode: SettingsMode, post_processor: &str) -> WhisperDictateApp {
    let mut app = test_app(AppSettings {
        ui_settings_mode: mode.id().to_owned(),
        post_processor: post_processor.to_owned(),
        ..AppSettings::default()
    });
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app
}

/// Advanced-only Post rows, by their rendered label.
const ADVANCED_ONLY_POST_ROWS: &[&str] = &[
    "Post model",
    "Post base URL",
    "Post timeout ms",
    "Post max input chars",
    "Post max output chars",
    "Cloud redaction",
    "Redaction terms",
];

#[test]
fn simple_mode_renders_only_the_two_essential_post_rows() {
    let mut app = app_in_mode(SettingsMode::Simple, "ollama");

    let texts = rendered_texts_for_tab(&mut app, Tab::Post);

    // The heading and both essential rows are there, so the tab is never an
    // empty group box under a dangling heading.
    assert!(contains_text(&texts, "Post-processing"));
    assert!(contains_text(&texts, "Post processor"));
    assert!(contains_text(&texts, "Post mode"));
    for hidden in ADVANCED_ONLY_POST_ROWS {
        assert!(
            !contains_text(&texts, hidden),
            "'{hidden}' must stay Advanced-only, got: {texts:?}"
        );
    }
}

#[test]
fn advanced_mode_still_renders_every_post_row() {
    let mut app = app_in_mode(SettingsMode::Advanced, "ollama");

    let texts = rendered_texts_for_tab(&mut app, Tab::Post);

    assert!(contains_text(&texts, "Post processor"));
    assert!(contains_text(&texts, "Post mode"));
    for visible in ADVANCED_ONLY_POST_ROWS {
        assert!(
            contains_text(&texts, visible),
            "'{visible}' must be visible in Advanced mode, got: {texts:?}"
        );
    }
}

/// The post-processing API-key block behaves like the STT one: shown in
/// Simple mode for a cloud processor (which cannot work without a key), and
/// absent for a local/disabled one.
#[test]
fn simple_mode_shows_the_post_api_key_block_only_for_a_cloud_processor() {
    let mut cloud = app_in_mode(SettingsMode::Simple, "groq");
    let cloud_texts = rendered_texts_for_tab(&mut cloud, Tab::Post);
    for expected in ["Post API key", "Save post API key", "Test post API"] {
        assert!(
            contains_text(&cloud_texts, expected),
            "'{expected}' must be reachable in Simple mode, got: {cloud_texts:?}"
        );
    }

    let mut local = app_in_mode(SettingsMode::Simple, "ollama");
    let local_texts = rendered_texts_for_tab(&mut local, Tab::Post);
    assert!(
        !contains_text(&local_texts, "Post API key"),
        "a local processor needs no key block, got: {local_texts:?}"
    );
}

// --- Opus review P1 (2026-09-16): a changed Post processor must retarget
// the endpoint immediately, not just at Save --------------------------------

/// Reachable purely through the two Simple-visible Post rows
/// (`post_processor`/`post_mode`) plus the Simple-visible API-key block:
/// before `apply_post_provider_change` called `normalize_postprocessor_settings`,
/// `post_base_url` (an Advanced-only, hence HIDDEN in Simple, row) stayed on
/// the OLD provider until an explicit Save, so a key pasted for the NEW
/// provider and sent via "Test post API" (or "Save post API key", both in
/// the Simple-visible block) went straight to the STALE endpoint — e.g. a
/// Groq key posted to `api.openai.com`. Mirrors
/// `settings_reset_tests::cloud_api_check_after_simple_reset_targets_the_reset_providers_endpoint`,
/// which proves the equivalent STT-side guarantee.
#[test]
fn post_provider_change_targets_the_new_providers_endpoint_with_no_save() {
    let mut app = test_app(AppSettings {
        post_processor: "openai".to_owned(),
        post_base_url: OPENAI_STT_BASE_URL.to_owned(),
        ..AppSettings::default()
    });
    let previous_post_provider = PostProvider::from_settings(&app.settings);

    // What the "Post processor" combo already wrote into
    // `self.settings.post_processor` by the time `post_processing_tab`
    // reaches this same call — no explicit Save anywhere in this test.
    app.settings.post_processor = "groq".to_owned();
    app.apply_post_provider_change(previous_post_provider);

    assert_eq!(
        app.settings.post_base_url, GROQ_STT_BASE_URL,
        "the endpoint must retarget the instant the processor changes"
    );
    let check =
        crate::cloud_api::PostApiCheck::from_settings(&app.settings, "gsk-test-key").unwrap();
    assert_eq!(check.provider, "Groq");
    assert_eq!(check.base_url, GROQ_STT_BASE_URL);
}

/// An UNCHANGED provider must not re-normalize on every render pass, or a
/// user's in-progress edit to `post_model`/`post_base_url` for the SAME
/// provider (e.g. a self-hosted proxy in front of Groq) would be fought
/// every frame.
#[test]
fn post_provider_unchanged_does_not_renormalize_the_endpoint() {
    const CUSTOM_GROQ_PROXY: &str = "https://my-groq-proxy.example.com/v1";
    let mut app = test_app(AppSettings {
        post_processor: "groq".to_owned(),
        post_base_url: CUSTOM_GROQ_PROXY.to_owned(),
        ..AppSettings::default()
    });
    let previous_post_provider = PostProvider::from_settings(&app.settings);

    app.apply_post_provider_change(previous_post_provider);

    assert_eq!(app.settings.post_base_url, CUSTOM_GROQ_PROXY);
}
