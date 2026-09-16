//! Tests for the content-driven sidebar width (#895 follow-up, Opus/user
//! review, 2026-09-16): pure-function proofs of `sidebar_content_width`'s
//! three guarantees (covers its content, never below the legacy floor,
//! never above the window-fraction cap, and deterministic), plus headless
//! render proofs that nothing actually elides at a normal window size and
//! that the elide/stack fallback DOES engage once the window is capped.

use super::render_test_support::painted_texts_at;
use super::test_support::test_app;
use super::*;

/// A headless `egui::Context` with fonts loaded — the minimum
/// `ctx.fonts_mut(...)` needs before it can lay out any text. One empty pass
/// is enough; nothing needs to be painted.
fn test_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(4000.0, 4000.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |_ui| {});
    output.textures_delta.clear();
    ctx
}

/// The UI-selectable text-scale range's endpoints plus three points in
/// between (`text_scale.rs`'s `[0.85, 1.6]` clamp), matching the exact set
/// the Opus review measured eliding at.
const SCALES: &[&str] = &["0.85", "1.0", "1.15", "1.3", "1.6"];
const LANGUAGES: &[&str] = &["en", "da"];
const MODES: &[SettingsMode] = &[SettingsMode::Simple, SettingsMode::Advanced];

// --- pure-function tests ----------------------------------------------------

/// A window wide enough that `MAX_WINDOW_WIDTH_FRACTION` never binds, so
/// these tests isolate the "covers its content" guarantee from the cap.
const GENEROUS_WINDOW: f32 = 5000.0;

#[test]
fn content_width_covers_every_element_it_measures_at_every_scale_and_language() {
    let ctx = test_ctx();
    for &raw_scale in SCALES {
        let scale = layout_scale(raw_scale);
        let button_padding_x = 10.0 * scale;
        for &raw_language in LANGUAGES {
            for &mode in MODES {
                let width =
                    sidebar_content_width(&ctx, mode, raw_language, raw_scale, GENEROUS_WINDOW);
                let inner = width - 2.0 * SIDEBAR_INNER_MARGIN;
                let header = header_content_width(&ctx, scale);
                let selector =
                    mode_selector_content_width(&ctx, raw_language, scale, button_padding_x);
                let nav = visible_tabs_content_width(&ctx, mode, raw_language, button_padding_x);
                assert!(
                    inner >= header - 0.01,
                    "scale={raw_scale} lang={raw_language} mode={mode:?}: inner {inner} < header need {header}"
                );
                assert!(
                    inner >= selector - 0.01,
                    "scale={raw_scale} lang={raw_language} mode={mode:?}: inner {inner} < selector need {selector}"
                );
                assert!(
                    inner >= nav - 0.01,
                    "scale={raw_scale} lang={raw_language} mode={mode:?}: inner {inner} < nav need {nav}"
                );
            }
        }
    }
}

#[test]
fn content_width_never_drops_below_the_legacy_floor() {
    let ctx = test_ctx();
    // Sweep window widths on both sides of the floor, including absurdly
    // tiny ones, so the floor guarantee holds even when the window-fraction
    // cap would otherwise ask for less.
    for &window_width in &[10.0_f32, 100.0, 300.0, GENEROUS_WINDOW] {
        for &raw_scale in SCALES {
            for &raw_language in LANGUAGES {
                for &mode in MODES {
                    let width =
                        sidebar_content_width(&ctx, mode, raw_language, raw_scale, window_width);
                    let floor = sidebar_width(raw_scale);
                    assert!(
                        width >= floor - 0.01,
                        "scale={raw_scale} window={window_width}: {width} < legacy floor {floor}"
                    );
                }
            }
        }
    }
}

#[test]
fn content_width_never_exceeds_the_window_fraction_cap() {
    let ctx = test_ctx();
    for &window_width in &[300.0_f32, 500.0, 800.0, GENEROUS_WINDOW] {
        for &raw_scale in SCALES {
            let floor = sidebar_width(raw_scale);
            let cap = (window_width * MAX_WINDOW_WIDTH_FRACTION).max(floor);
            for &raw_language in LANGUAGES {
                for &mode in MODES {
                    let width =
                        sidebar_content_width(&ctx, mode, raw_language, raw_scale, window_width);
                    assert!(
                        width <= cap + 0.01,
                        "scale={raw_scale} window={window_width} lang={raw_language} mode={mode:?}: {width} > cap {cap}"
                    );
                }
            }
        }
    }
}

/// The oscillation guard: `app.rs` feeds this value straight into the
/// panel's `exact_size`, so if the SAME inputs ever produced a DIFFERENT
/// output the panel width (and therefore the next frame's inputs, if this
/// were ever fed back in) would drift frame to frame.
#[test]
fn content_width_is_a_pure_function_of_its_inputs() {
    let ctx = test_ctx();
    for &raw_scale in SCALES {
        for &raw_language in LANGUAGES {
            for &mode in MODES {
                let first =
                    sidebar_content_width(&ctx, mode, raw_language, raw_scale, GENEROUS_WINDOW);
                let second =
                    sidebar_content_width(&ctx, mode, raw_language, raw_scale, GENEROUS_WINDOW);
                assert_eq!(
                    first, second,
                    "scale={raw_scale} lang={raw_language} mode={mode:?}"
                );
            }
        }
    }
    // Also across two INDEPENDENT contexts: egui's default font data is
    // loaded deterministically, so two fresh contexts must measure text
    // identically — this is what makes it safe for `app.rs` to call this
    // once per frame off its own live `ctx` without ever caching a result.
    let ctx_a = test_ctx();
    let ctx_b = test_ctx();
    let a = sidebar_content_width(&ctx_a, SettingsMode::Advanced, "da", "1.3", GENEROUS_WINDOW);
    let b = sidebar_content_width(&ctx_b, SettingsMode::Advanced, "da", "1.3", GENEROUS_WINDOW);
    assert_eq!(a, b);
}

/// Nav cards all share the sidebar's content width (`nav_button` fills
/// `ui.available_width()`), so "every card fits the longest label" reduces
/// to: the shared width covers the WIDEST currently-visible tab in EACH
/// mode. Advanced (8 tabs, including the longer "Dictionary"/"Profiles")
/// needs at least as much nav width as Simple (4 tabs, a subset), and both
/// are covered by `sidebar_content_width`'s own output.
#[test]
fn nav_width_fits_the_longest_visible_tab_in_both_simple_and_advanced() {
    let ctx = test_ctx();
    for &raw_scale in SCALES {
        let scale = layout_scale(raw_scale);
        let button_padding_x = 10.0 * scale;
        for &raw_language in LANGUAGES {
            let simple_nav = visible_tabs_content_width(
                &ctx,
                SettingsMode::Simple,
                raw_language,
                button_padding_x,
            );
            let advanced_nav = visible_tabs_content_width(
                &ctx,
                SettingsMode::Advanced,
                raw_language,
                button_padding_x,
            );
            assert!(
                advanced_nav >= simple_nav - 0.01,
                "scale={raw_scale} lang={raw_language}: Advanced's widest tab ({advanced_nav}) \
                 must be at least Simple's ({simple_nav}) since Simple's tabs are a subset"
            );
            for &mode in MODES {
                let width =
                    sidebar_content_width(&ctx, mode, raw_language, raw_scale, GENEROUS_WINDOW);
                let nav_need =
                    visible_tabs_content_width(&ctx, mode, raw_language, button_padding_x);
                assert!(width - 2.0 * SIDEBAR_INNER_MARGIN >= nav_need - 0.01);
            }
        }
    }
}

// --- headless render tests ---------------------------------------------------

fn app_for_render(raw_scale: &str, raw_language: &str, mode: SettingsMode) -> WhisperDictateApp {
    let mut app = test_app(AppSettings {
        ui_text_scale: raw_scale.to_owned(),
        ui_language: raw_language.to_owned(),
        ui_settings_mode: mode.id().to_owned(),
        ..AppSettings::default()
    });
    app.audio_devices_loaded = true;
    app.settings.update_check = false;
    app.tray.disable();
    app
}

/// A window comfortably wider than any real content need at any scale, so
/// the window-fraction cap never engages — isolating "does normal-size
/// rendering elide anything" from the deliberately-capped test below.
const NORMAL_WINDOW: egui::Vec2 = egui::vec2(1400.0, 1400.0);

/// At scale 0.85, 1.0 and 1.15 in a normal-width window: the header title,
/// BOTH Simple/Advanced selector labels, and every currently-visible nav
/// label paint IN FULL — no ellipsis anywhere. Uses the glyph-run assertion
/// (`painted_texts_at`/`render_test_support::painted_glyph_text`), which
/// reports what was ACTUALLY painted (including any elision), not the
/// original layout-job string.
#[test]
fn nothing_elides_at_normal_scales_in_a_normal_width_window() {
    for &raw_scale in &["0.85", "1.0", "1.15"] {
        for &raw_language in LANGUAGES {
            for &mode in MODES {
                let mut app = app_for_render(raw_scale, raw_language, mode);
                let painted = painted_texts_at(&mut app, NORMAL_WINDOW);
                let painted_strings: Vec<&str> = painted.iter().map(|p| p.text.as_str()).collect();

                assert!(
                    painted_strings.contains(&"whisper-dictate"),
                    "scale={raw_scale} lang={raw_language} mode={mode:?}: title elided, painted: {painted_strings:?}"
                );
                for selector_mode in SettingsMode::ALL {
                    let label = selector_mode.label(raw_language);
                    assert!(
                        painted_strings.contains(&label),
                        "scale={raw_scale} lang={raw_language} mode={mode:?}: {label:?} elided, painted: {painted_strings:?}"
                    );
                }
                for tab in Tab::ALL {
                    if !tab_visible(mode, tab) {
                        continue;
                    }
                    let expected = format!("{}  {}", tab.icon(), tab.label(raw_language));
                    assert!(
                        painted_strings.contains(&expected.as_str()),
                        "scale={raw_scale} lang={raw_language} mode={mode:?}: nav tab {:?} \
                         elided or missing, painted: {painted_strings:?}",
                        tab.label(raw_language)
                    );
                }
            }
        }
    }
}

/// The one case eliding is correct: a window too narrow for the
/// content-driven width to fit under its window-fraction cap. Derives the
/// capped window width from the sidebar's OWN uncapped ("natural") width
/// rather than a guessed pixel constant, so the test stays correct
/// regardless of exact font metrics — it only needs the cap to bind
/// somewhere below the natural width, which `content_width_never_exceeds_the_window_fraction_cap`
/// already proves happens for a small enough window.
#[test]
fn the_fallback_engages_without_spilling_past_the_panel_when_the_window_is_capped() {
    const RAW_SCALE: &str = "1.15";
    const RAW_LANGUAGE: &str = "en";
    const MODE: SettingsMode = SettingsMode::Advanced;

    let ctx = test_ctx();
    let natural = sidebar_content_width(&ctx, MODE, RAW_LANGUAGE, RAW_SCALE, GENEROUS_WINDOW);
    // Pick a window whose 45% cap lands at 80% of the natural width — comfortably
    // below "wanted" so the cap actually binds, comfortably above zero so the
    // clamp still has room to work with.
    let window_width = (natural * 0.8) / MAX_WINDOW_WIDTH_FRACTION;
    let capped_panel_width =
        sidebar_content_width(&ctx, MODE, RAW_LANGUAGE, RAW_SCALE, window_width);
    assert!(
        capped_panel_width < natural - 0.01,
        "the cap must actually bind here: capped {capped_panel_width} vs natural {natural}"
    );

    let mut app = app_for_render(RAW_SCALE, RAW_LANGUAGE, MODE);
    let painted = painted_texts_at(&mut app, egui::vec2(window_width, 1400.0));
    let inner_right = capped_panel_width - SIDEBAR_INNER_MARGIN;

    // The title either fits in full or ends in an ellipsis (never silently
    // missing or mid-glyph clipped), and never spills past the capped
    // panel's own right edge.
    let title = painted
        .iter()
        .find(|p| p.text.starts_with("whisper"))
        .expect("the title must still be painted, elided or not");
    assert!(
        title.text == "whisper-dictate" || title.text.ends_with('…'),
        "unexpected title paint: {:?}",
        title.text
    );
    assert!(
        title.rect.right() <= inner_right + 0.5,
        "title spilled past the capped sidebar's right edge: rect={:?}, inner_right={inner_right}",
        title.rect
    );

    // Both selector labels: each is either painted in full, elided to a
    // prefix + "…", or (under heavy elision) reduced to a bare "…" — never
    // silently missing — and whatever paints for it never spills past the
    // capped panel's own right edge, whether it ended up side by side or
    // stacked into full-width rows.
    let is_elision_of = |painted_text: &str, label: &str| {
        painted_text == label
            || (painted_text.ends_with('…')
                && label.starts_with(painted_text.trim_end_matches('…')))
    };
    for selector_mode in SettingsMode::ALL {
        let label = selector_mode.label(RAW_LANGUAGE);
        let painted_label = painted
            .iter()
            .find(|p| is_elision_of(&p.text, label))
            .unwrap_or_else(|| {
                panic!(
                    "{label:?} not painted at all (in full or elided), painted: {:?}",
                    painted.iter().map(|p| &p.text).collect::<Vec<_>>()
                )
            });
        assert!(
            painted_label.rect.right() <= inner_right + 0.5,
            "{label:?} spilled past the capped sidebar's right edge: rect={:?}, inner_right={inner_right}",
            painted_label.rect
        );
    }
}
