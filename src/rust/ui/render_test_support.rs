//! Shared headless-rendering test harness.
//!
//! `egui::Context::run_ui` drives a full `eframe::App::ui` pass through pure
//! CPU text layout — no window, GPU, or `egui_kittest` dependency needed —
//! and returns the literal strings actually painted that frame. Tests use
//! this to assert which labels/rows a tab genuinely rendered, proving the
//! Simple/Advanced gating is applied by the renderer itself rather than only
//! self-consistent in the pure `setting_visible`/`tab_visible` rules: if a
//! renderer stops calling a gate, the painted text changes and a test here
//! catches it.

use super::*;
use std::collections::HashSet;

/// Render `app` for one frame (whatever `app.selected_tab` / other state
/// currently is) and return every literal string painted.
///
/// The virtual screen is absurdly tall (not a real window size — nothing
/// draws it) so every row of even the longest settings page (Speech,
/// System) stays inside the settings body's `ScrollArea` clip rect. Widgets
/// egui judges off-screen via `Ui::is_rect_visible` skip galley shaping
/// entirely as a perf optimization, so a realistic ~1000px window height
/// would silently cull — not just visually clip — every row past the fold,
/// making a render test that only checked the first screenful.
pub(super) fn rendered_texts(app: &mut WhisperDictateApp) -> HashSet<String> {
    let ctx = egui::Context::default();
    let mut frame = eframe::Frame::_new_kittest();
    let mut input = egui::RawInput::default();
    input.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(1400.0, 40_000.0),
    ));
    let mut output = ctx.run_ui(input, |ui| {
        eframe::App::ui(app, ui, &mut frame);
    });
    // No real painter consumes this headless frame, so drop the pending
    // font-atlas texture delta explicitly instead of letting `TexturesDelta`
    // debug-assert on an unapplied delta at drop time.
    output.textures_delta.clear();
    let mut texts = HashSet::new();
    collect_shapes(&output.shapes, &mut texts);
    texts
}

/// Select `tab` and render `app`, returning every literal string painted.
pub(super) fn rendered_texts_for_tab(app: &mut WhisperDictateApp, tab: Tab) -> HashSet<String> {
    app.selected_tab = tab;
    rendered_texts(app)
}

fn collect_shapes(shapes: &[egui::epaint::ClippedShape], out: &mut HashSet<String>) {
    for clipped in shapes {
        collect_shape(&clipped.shape, out);
    }
}

fn collect_shape(shape: &egui::Shape, out: &mut HashSet<String>) {
    match shape {
        egui::Shape::Vec(inner) => {
            for s in inner {
                collect_shape(s, out);
            }
        }
        egui::Shape::Text(text_shape) => {
            out.insert(text_shape.galley.text().to_owned());
        }
        _ => {}
    }
}

/// A test helper asserting `needle` is (or is not) among a set of rendered
/// strings, matched by substring so callers don't need the exact/whole
/// painted string (which may combine an icon glyph + label in one galley).
pub(super) fn contains_text(texts: &HashSet<String>, needle: &str) -> bool {
    texts.iter().any(|text| text.contains(needle))
}

#[cfg(test)]
mod self_test {
    use super::super::test_support::test_app;
    use super::*;

    #[test]
    fn harness_renders_the_output_tab_and_finds_its_label() {
        let mut app = test_app(AppSettings::default());
        app.audio_devices_loaded = true;
        app.settings.update_check = false;
        app.tray.disable();

        let texts = rendered_texts_for_tab(&mut app, Tab::Output);

        assert!(
            contains_text(&texts, "Inject mode"),
            "expected 'Inject mode' among rendered texts, got: {texts:?}"
        );
    }
}
