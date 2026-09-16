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
    painted_texts_at(app, egui::vec2(1400.0, 40_000.0))
        .into_iter()
        .map(|painted| painted.text)
        .collect()
}

/// One text galley the frame actually painted, with the geometry needed to
/// prove it stayed inside its panel.
pub(super) struct PaintedText {
    /// The literal string in the galley.
    pub(super) text: String,
    /// Screen-space rect the galley occupies.
    pub(super) rect: egui::Rect,
}

/// Render `app` for one frame at an explicit virtual screen size and return
/// every painted text galley with its screen rect.
///
/// Layout regressions that only show up in a narrow window (#895) need a
/// REALISTIC width — the tall-and-wide screen [`rendered_texts`] uses can
/// never clip the sidebar — plus each galley's rect, so a test can assert the
/// painted text stayed inside the sidebar rather than spilling past its edge.
pub(super) fn painted_texts_at(
    app: &mut WhisperDictateApp,
    screen: egui::Vec2,
) -> Vec<PaintedText> {
    let ctx = egui::Context::default();
    let mut frame = eframe::Frame::_new_kittest();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        eframe::App::ui(app, ui, &mut frame);
    });
    // No real painter consumes this headless frame, so drop the pending
    // font-atlas texture delta explicitly instead of letting `TexturesDelta`
    // debug-assert on an unapplied delta at drop time.
    output.textures_delta.clear();
    let mut texts = Vec::new();
    collect_shapes(&output.shapes, &mut texts);
    texts
}

/// Run one headless pass over a `Ui` that is exactly `size` big and return the
/// rect `add_contents` actually occupied (`Ui::min_rect`).
///
/// For a widget whose whole job is to fit inside a fixed-width panel, this is
/// the sharpest possible assertion: egui lets `min_rect` grow PAST `max_rect`
/// when a widget asks for more room than the panel has, and then clips the
/// overflow at the panel edge — which is exactly the #895 symptom. Driving the
/// widget in a panel-sized `Ui` therefore surfaces the overflow as a number,
/// where a full-window render only shows the already-clipped result.
pub(super) fn measure_in_panel<R>(
    size: egui::Vec2,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Rect {
    let ctx = egui::Context::default();
    let panel = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    let input = egui::RawInput {
        screen_rect: Some(panel),
        ..Default::default()
    };
    // `run_ui` wants an `FnMut`, so hand the `FnOnce` body over through an
    // Option the first (and only) pass takes.
    let mut add_contents = Some(add_contents);
    let mut used = egui::Rect::NOTHING;
    let mut output = ctx.run_ui(input, |ui| {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(panel));
        if let Some(add_contents) = add_contents.take() {
            add_contents(&mut child);
        }
        used = child.min_rect();
    });
    output.textures_delta.clear();
    used
}

/// Select `tab` and render `app`, returning every literal string painted.
pub(super) fn rendered_texts_for_tab(app: &mut WhisperDictateApp, tab: Tab) -> HashSet<String> {
    app.selected_tab = tab;
    rendered_texts(app)
}

/// The text a galley actually PAINTS, reconstructed glyph-by-glyph, as
/// opposed to `Galley::text()` (`&self.job.text`), which is the ORIGINAL
/// layout-job string regardless of what ended up on screen.
///
/// Opus review P2 (2026-09-16): `both_mode_labels_are_still_painted_in_a_narrow_sidebar`
/// asserted `painted.text == label` using `galley.text()`, so a galley that
/// actually rendered "Adva…" (elided by `.truncate()`) still reported the
/// full, untruncated `"Advanced"` — the assertion could never fail no
/// matter how badly a label was clipped, which is exactly why the whole
/// #895 suite passed against the still-broken P1-1 code. `PlacedRow`
/// (`Galley::rows`) derefs to `Row`, whose `glyphs` are the actual laid-out
/// characters — including the substituted `…` and excluding whatever
/// `.truncate()` dropped — so concatenating them reports what a person
/// looking at the screen would actually read.
fn painted_glyph_text(galley: &egui::Galley) -> String {
    galley
        .rows
        .iter()
        .flat_map(|row| row.glyphs.iter().map(|glyph| glyph.chr))
        .collect()
}

fn collect_shapes(shapes: &[egui::epaint::ClippedShape], out: &mut Vec<PaintedText>) {
    for clipped in shapes {
        collect_shape(&clipped.shape, out);
    }
}

fn collect_shape(shape: &egui::Shape, out: &mut Vec<PaintedText>) {
    match shape {
        egui::Shape::Vec(inner) => {
            for s in inner {
                collect_shape(s, out);
            }
        }
        egui::Shape::Text(text_shape) => {
            out.push(PaintedText {
                text: painted_glyph_text(&text_shape.galley),
                rect: text_shape.galley.rect.translate(text_shape.pos.to_vec2()),
            });
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
