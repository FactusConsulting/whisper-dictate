use super::{egui, WhisperDictateApp};
use crate::cloud_api::hviske_local::LocalModel;
use crate::ui::{test_support::test_app, AppSettings};

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

#[test]
fn local_hviske_presets_require_a_fitting_cuda_gpu_and_the_license_notice() {
    let ctx = egui::Context::default();
    let mut app = test_app(AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "custom".to_owned(),
        stt_model: "existing-custom-model".to_owned(),
        stt_base_url: "https://untouched.example/v1".to_owned(),
        ..AppSettings::default()
    });
    let labels = frame(&ctx, &mut app, Vec::new());
    assert!(labels.iter().any(|(text, _)| text == "Hviske v6"));
    assert!(labels.iter().all(|(text, _)| !text.contains("local GPU")));

    app.hviske_gpu_total_mib = Some(LocalModel::V6.required_mib());
    let labels = frame(&ctx, &mut app, Vec::new());
    assert!(labels
        .iter()
        .any(|(text, _)| text == "Hviske v6 (local GPU)"));
    assert!(labels
        .iter()
        .all(|(text, _)| text != "Hviske v5.3 (local GPU)"));

    click(&ctx, &mut app, "Hviske v6 (local GPU)");
    assert_eq!(app.settings.stt_model, "existing-custom-model");
    let notice = frame(&ctx, &mut app, Vec::new());
    for required in [
        "Use Hviske v6 (local GPU)?",
        "Audio stays on this PC",
        "no CPU fallback",
    ] {
        assert!(notice.iter().any(|(text, _)| text.contains(required)));
    }
    click(&ctx, &mut app, "Use model");
    assert_eq!(app.settings.stt_model, LocalModel::V6.alias());
    assert_eq!(app.settings.stt_base_url, "https://untouched.example/v1");
    assert!(!app.cloud_stt_missing_api_key());
    assert!(!app.settings.local_only);

    app.hviske_gpu_total_mib = None;
    let labels = frame(&ctx, &mut app, Vec::new());
    assert!(labels
        .iter()
        .any(|(text, _)| text.starts_with("Unavailable:")));
    assert!(!app.local_hviske_can_test());
}
