//! view responsibilities for the desktop controller.

use super::*;

impl eframe::App for WhisperDictateApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.run_non_visual_logic(ctx);
    }

    // egui 0.34 renamed the required `App` method from `update(&Context, ..)` to
    // `ui(&mut Ui, ..)`. eframe 0.36 now calls `logic` before this method and
    // uses `logic` alone while the viewport is hidden. Rendering and live egui
    // input therefore stay here; lifecycle polling lives above.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Bind the cloned `Context` into a named owned local (not a reference to a
        // temporary) so the borrow split off `ui` is explicit and not reliant on
        // temporary lifetime extension; pass it as `&ctx` where a `&Context` is
        // needed.
        let ctx = ui.ctx().clone();
        // Hidden logic sees the last visible frame's input, so consume hotkey
        // capture events only during a real egui pass.
        self.poll_hotkey_capture(&ctx);
        // Launch-time auto-start (`ui_autostart_runtime`): fires on the pass
        // AFTER the first painted frame, so the window is up and the runtime
        // log is on screen before the start (or its skip reason) is reported.
        // One shot per session — see `ui/autostart.rs`. Deliberately driven
        // from the render pass and not from `run_non_visual_logic` (which
        // also runs while the viewport is hidden) nor from `App::new`, and
        // placed ahead of the compact-mode early return so that branch cannot
        // strand the one-shot.
        self.poll_autostart_runtime();
        let palette = ui_palette(&self.settings.ui_theme);
        apply_ui_theme(&ctx, &self.settings.ui_text_scale, &self.settings.ui_theme);

        // Compact mode: a single tiny CentralPanel with one control row — no
        // sidebar, tabs, log, or message bars. The viewport is already resized /
        // raised always-on-top by `set_compact_mode`; here we only render.
        if self.compact_mode {
            egui::CentralPanel::default()
                .frame(egui::Frame::default().fill(palette.panel_bg).inner_margin(
                    egui::Margin::symmetric(EDGE_MARGIN as i8, EDGE_MARGIN as i8),
                ))
                .show(ui, |ui| self.compact_panel(ui, palette));
            return;
        }

        // Content-driven sidebar width (#895 follow-up): measured from font
        // metrics BEFORE the panel exists, from `ui.available_width()` here
        // — the FULL window's content width, since no panel has claimed any
        // of it yet. Reading a NARROWER width (e.g. after the sidebar itself
        // existed) would feed the panel's own output back into the value
        // that decides its width and oscillate; see `sidebar_width.rs`.
        let sidebar_mode = SettingsMode::from_raw(&self.settings.ui_settings_mode);
        let sidebar_panel_width = sidebar_content_width(
            &ctx,
            sidebar_mode,
            &self.settings.ui_language,
            &self.settings.ui_text_scale,
            ui.available_width(),
        );
        paint_sidebar_bridge(&ctx, palette, sidebar_panel_width);

        egui::Panel::left("primary_navigation")
            .resizable(false)
            .show_separator_line(false)
            .exact_size(sidebar_panel_width)
            .frame(
                egui::Frame::default()
                    .fill(palette.header_bg)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(
                        SIDEBAR_INNER_MARGIN as i8,
                        SIDEBAR_INNER_MARGIN as i8,
                    )),
            )
            .show(ui, |ui| self.sidebar(ui, palette));

        egui::Panel::top("runtime_status")
            .resizable(false)
            .exact_size(top_status_bar_height(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.panel_bg)
                    .stroke(egui::Stroke::new(0.8, palette.border_soft))
                    .inner_margin(egui::Margin::symmetric(16, TOP_PANEL_V_MARGIN as i8)),
            )
            .show(ui, |ui| self.top_status_bar(ui, palette));

        // Thin global status bar: saved/unsaved state + the latest message,
        // on every tab, replacing the per-page Messages card.
        egui::Panel::bottom("status_message_bar")
            .resizable(false)
            .exact_size(bottom_message_bar_height(&self.settings.ui_text_scale))
            .frame(
                egui::Frame::default()
                    .fill(palette.header_bg)
                    .stroke(egui::Stroke::new(0.8, palette.border_soft))
                    // Match the central panel's left inset so the status dot lines
                    // up with the content above it.
                    .inner_margin(egui::Margin::symmetric(EDGE_MARGIN as i8, 4)),
            )
            .show(ui, |ui| self.status_message_bar(ui, palette));

        // Backstop for every path that can change the settings mode without
        // going through `select_tab` (e.g. `reload_settings` reading a
        // "simple" config off disk, or a `wd config set` edit taking effect
        // after such a reload): re-clamp `selected_tab` unconditionally every
        // frame, right before it decides what to render, so a hidden tab
        // (Quality/Dictionary/Post/Profiles in Simple mode) can never stay
        // selected.
        self.select_tab(self.selected_tab);

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(palette.panel_bg).inner_margin(
                egui::Margin::symmetric(EDGE_MARGIN as i8, EDGE_MARGIN as i8),
            ))
            .show(ui, |ui| match self.selected_tab {
                Tab::Log => self.runtime_tab(ui),
                Tab::Speech => self.settings_panel(ui, Self::core_tab),
                Tab::Quality => self.settings_panel(ui, Self::quality_tab),
                Tab::Dictionary => self.settings_panel(ui, Self::dictionary_tab),
                Tab::Output => self.settings_panel(ui, Self::output_tab),
                Tab::Post => self.settings_panel(ui, Self::post_processing_tab),
                Tab::Profiles => self.settings_panel(ui, Self::profiles_tab),
                Tab::System => self.settings_panel(ui, Self::system_tab),
            });
    }
}
