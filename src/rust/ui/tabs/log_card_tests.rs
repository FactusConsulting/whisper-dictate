//! The transcript copy button on a runtime log card, driven through a
//! headless egui pass: the button is painted only on transcript cards, a
//! click on it returns the card's text, and a disabled button returns none.

use super::super::*;
use super::log_card::runtime_log_card;
use egui_material_icons::icons;

fn card(kind: RuntimeLogCardKind, title: &str, badge: &str) -> RuntimeLogCard {
    RuntimeLogCard {
        kind,
        title: title.to_owned(),
        detail: String::new(),
        badge: badge.to_owned(),
    }
}

fn screen() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 400.0))
}

/// Run one frame of `card` and return what the card reported plus where the
/// copy glyph was painted, if it was.
fn frame(
    ctx: &egui::Context,
    card: &RuntimeLogCard,
    enabled: bool,
    events: Vec<egui::Event>,
) -> (Option<String>, Option<egui::Pos2>) {
    let palette = ui_palette("dark");
    let mut copied = None;
    let input = egui::RawInput {
        screen_rect: Some(screen()),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        copied =
            runtime_log_card(ui, card, "", palette, Some((enabled, "Copy"))).map(str::to_owned);
    });
    output.textures_delta.clear();
    let glyph = output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.text() == icons::ICON_CONTENT_COPY.codepoint => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            _ => None,
        });
    (copied, glyph)
}

fn click(at: egui::Pos2) -> Vec<Vec<egui::Event>> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    vec![
        vec![egui::Event::PointerMoved(at)],
        vec![button(true)],
        vec![button(false)],
    ]
}

/// Render `card`, click where its copy glyph was painted, and return what the
/// card reported on the frame of the click.
fn click_copy(card: &RuntimeLogCard, enabled: bool) -> Option<String> {
    let ctx = egui::Context::default();
    let (_, glyph) = frame(&ctx, card, enabled, Vec::new());
    let at = glyph.expect("a transcript card paints its copy glyph");
    let mut copied = None;
    for events in click(at) {
        copied = copied.or(frame(&ctx, card, enabled, events).0);
    }
    copied
}

#[test]
fn a_click_on_the_copy_button_returns_the_transcript() {
    let final_text = card(RuntimeLogCardKind::FinalText, "hello world", "Final");
    assert_eq!(
        click_copy(&final_text, true).as_deref(),
        Some("hello world")
    );
    let dictation = card(RuntimeLogCardKind::Diagnostic, "spoken text", "Utterance");
    assert_eq!(click_copy(&dictation, true).as_deref(), Some("spoken text"));
}

#[test]
fn a_disabled_copy_button_returns_nothing() {
    let final_text = card(RuntimeLogCardKind::FinalText, "hello world", "Final");
    assert_eq!(click_copy(&final_text, false), None);
}

#[test]
fn cards_without_a_transcript_paint_no_copy_button() {
    let ctx = egui::Context::default();
    for card in [
        card(RuntimeLogCardKind::Status, "Worker started", "Worker"),
        card(RuntimeLogCardKind::HealthFair, "noisy", "HealthFair"),
    ] {
        assert_eq!(frame(&ctx, &card, true, Vec::new()).1, None, "{card:?}");
    }
    let final_text = card(RuntimeLogCardKind::FinalText, "hello", "Final");
    assert!(frame(&ctx, &final_text, true, Vec::new()).1.is_some());
    // Without a copy configuration a transcript card paints none either.
    let palette = ui_palette("dark");
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(screen()),
            ..Default::default()
        },
        |ui| {
            runtime_log_card(ui, &final_text, "", palette, None);
        },
    );
    output.textures_delta.clear();
    assert!(!output.shapes.iter().any(|clipped| matches!(
        &clipped.shape,
        egui::Shape::Text(text) if text.galley.text() == icons::ICON_CONTENT_COPY.codepoint
    )));
}
