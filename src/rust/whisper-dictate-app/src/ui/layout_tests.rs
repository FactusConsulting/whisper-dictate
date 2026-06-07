use super::test_support::test_app;
use super::*;
use crate::config::AppSettings;

#[test]
fn shell_chrome_dimensions_scale_with_ui_text() {
    assert!(top_status_bar_height("1.15") >= 73.0);
    assert_eq!(top_status_bar_height("bad"), 64.0);
    assert_eq!(top_status_bar_height("3.0"), 102.4);
    assert!(sidebar_width("1.15") >= 188.0);
    assert_eq!(sidebar_width("bad"), 164.0);
    assert!((sidebar_width("3.0") - 262.4).abs() < 0.001);
}

/// Render the settings footer headlessly inside a central panel of the given
/// window width and return `(panel_inner_width, messages_card_width)`.
fn measure_messages_card(window_width: f32, status: &str) -> (f32, f32) {
    let mut app = test_app(AppSettings::default());
    app.selected_tab = Tab::Speech;
    app.settings_status = status.to_owned();

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(window_width, 760.0),
        )),
        ..Default::default()
    };

    let mut measured = (0.0_f32, 0.0_f32);
    // Two passes: egui needs a warm-up frame to settle galley/scroll sizing.
    for _ in 0..2 {
        let _ = ctx.run(raw_input.clone(), |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(12.0, 12.0)))
                .show(ctx, |ui| {
                    let panel_width = ui.available_width();
                    ui.separator();
                    let card_width = ui
                        .allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), 264.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                ui.set_min_width(ui.available_width());
                                app.footer_messages_card_width(ui)
                            },
                        )
                        .inner;
                    measured = (panel_width, card_width);
                });
        });
    }
    measured
}

#[test]
fn messages_card_fills_available_width_on_resize() {
    // The card must span the full panel width at every window size (no growing
    // gap on the right) for both populated and empty message states.
    for status in ["A status message that is reasonably long.", ""] {
        for window_width in [820.0_f32, 1100.0, 1500.0, 1920.0] {
            let (panel_width, card_width) = measure_messages_card(window_width, status);
            let gap = panel_width - card_width;
            assert!(
                gap.abs() < 0.5,
                "messages card should fill the panel width (window={window_width}, status_empty={}): panel={panel_width} card={card_width} gap={gap}",
                status.is_empty(),
            );
        }
    }
}
