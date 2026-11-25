use super::*;
use crate::ui::test_support::test_app;

fn collect_text(shape: &egui::Shape, painted: &mut Vec<(String, egui::Pos2)>) {
    match shape {
        egui::Shape::Text(text) => painted.push((
            text.galley.text().to_owned(),
            text.pos + text.galley.rect.center().to_vec2(),
        )),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(shape, painted);
            }
        }
        _ => {}
    }
}

fn frame(
    ctx: &egui::Context,
    app: &mut WhisperDictateApp,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Pos2)> {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 600.0),
        )),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        egui::Grid::new("hviske-test-grid").show(ui, |ui| {
            app.hviske_model_options(ui, true);
        });
    });
    output.textures_delta.clear();
    let mut painted = Vec::new();
    for clipped in &output.shapes {
        collect_text(&clipped.shape, &mut painted);
    }
    painted
}

fn click(ctx: &egui::Context, app: &mut WhisperDictateApp, label: &str) {
    let at = frame(ctx, app, Vec::new())
        .into_iter()
        .find_map(|(text, at)| (text == label).then_some(at))
        .unwrap_or_else(|| panic!("missing visible button {label:?}"));
    frame(ctx, app, vec![egui::Event::PointerMoved(at)]);
    for pressed in [true, false] {
        frame(
            ctx,
            app,
            vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            }],
        );
    }
}

#[test]
fn hviske_model_changes_only_after_accepting_the_license_and_audio_notice() {
    let ctx = egui::Context::default();
    let mut app = test_app(AppSettings {
        stt_model: "existing-custom-model".to_owned(),
        stt_base_url: "http://localhost:8000/v1".to_owned(),
        ..AppSettings::default()
    });

    click(&ctx, &mut app, "Hviske v5.3");
    assert_eq!(app.settings.stt_model, "existing-custom-model");
    let notice = frame(&ctx, &mut app, Vec::new());
    for required in [
        "Use Hviske v5.3?",
        "noncommercial use under CC BY-NC 4.0",
        "Dictation sends audio to that server",
    ] {
        assert!(notice.iter().any(|(text, _)| text.contains(required)));
    }
    click(&ctx, &mut app, "Cancel");
    assert_eq!(app.settings.stt_model, "existing-custom-model");

    click(&ctx, &mut app, "Hviske v6");
    assert_eq!(app.settings.stt_model, "existing-custom-model");
    click(&ctx, &mut app, "Use model");
    assert_eq!(app.settings.stt_model, crate::cloud_api::HVISKE_V6);
    assert_eq!(app.settings.stt_base_url, "http://localhost:8000/v1");
}
