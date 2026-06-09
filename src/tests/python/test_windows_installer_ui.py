from helpers import (
    Path,
    unittest,
)

def rust_ui_source():
    # ui.rs + every non-test .rs under ui/ (resilient to the tabs/ split).
    ui = Path("src/rust/ui")
    paths = [Path("src/rust/ui.rs")]
    paths += sorted(p for p in ui.rglob("*.rs") if not p.name.endswith("_tests.rs"))
    return "\n".join(p.read_text(encoding="utf-8") for p in paths)

def rust_config_source():
    # config.rs OR every .rs under config/ (resilient to the module split).
    src = Path("src/rust")
    single = src / "config.rs"
    paths = [single] if single.exists() else sorted((src / "config").rglob("*.rs"))
    return "\n".join(p.read_text(encoding="utf-8") for p in paths)

class WindowsLauncherRegressionTests(unittest.TestCase):
    def test_installer_no_longer_packages_legacy_launchers(self):
        with open("packaging/windows/inno/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        for legacy in (
            "setup.ps1",
            "setup.cmd",
            "settings-ui.ps1",
            "settings-ui.vbs",
            "requirements-ui.txt",
            "Legacy Settings UI",
            "whisper-dictate Terminal",
        ):
            self.assertNotIn(legacy, script)
        self.assertIn(r'Source: "..\..\..\target\release\whisper-dictate.exe"', script)
        self.assertIn(r'Source: "..\..\..\src\python\whisper_dictate\*.py"', script)
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"', script)

    def test_installer_closes_running_windows_app_before_upgrade(self):
        script = Path("packaging/windows/inno/whisper-dictate.iss").read_text(encoding="utf-8")

        self.assertIn("CloseApplications=yes", script)
        self.assertIn("RestartApplications=no", script)
        self.assertIn("function IsWhisperDictateRunning(): Boolean", script)
        self.assertIn("function StopRunningWhisperDictate(): String", script)
        self.assertIn("function CommandLineQuote(S: String): String", script)
        self.assertIn("'-NoProfile -ExecutionPolicy Bypass -File ' + CommandLineQuote(ScriptPath)", script)
        prepare = script.split("function PrepareToInstall", 1)[1].split(
            "procedure CurStepChanged", 1
        )[0]
        self.assertLess(
            prepare.index("if IsWhisperDictateRunning() then"),
            prepare.index("StopError := StopRunningWhisperDictate();"),
        )
        self.assertIn("Close it now so setup can continue?", prepare)
        self.assertIn("MB_YESNO", prepare)
        self.assertIn("IDYES", prepare)
        self.assertLess(
            prepare.index("StopError := StopRunningWhisperDictate();"),
            prepare.index("UninstallPrevious();"),
        )
        self.assertIn("MB_RETRYCANCEL", prepare)
        self.assertIn("IDRETRY", prepare)
        self.assertIn("Close whisper-dictate, then click Retry to continue.", prepare)
        self.assertIn("$_.ExecutablePath -eq $appExe", script)
        self.assertIn("CloseMainWindow()", script)
        self.assertIn("Stop-Process -Id $_.ProcessId -Force", script)
        self.assertIn("$deadline = (Get-Date).AddSeconds(10)", script)
        self.assertIn("whisper_dictate.runtime", script)
        self.assertIn("$_.CommandLine -like (''*'' + $appRoot + ''*'')", script)
        self.assertNotIn("Stop-Process -Name python", script)
        self.assertIn("Close whisper-dictate and run the installer again.", script)

    def test_rust_windows_ui_uses_gui_subsystem(self):
        script = Path("src/rust/main.rs").read_text(encoding="utf-8")

        self.assertIn(
            '#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]',
            script,
        )

    def test_rust_background_processes_hide_windows_console(self):
        script = Path("src/rust/runtime.rs").read_text(encoding="utf-8")

        self.assertIn("const CREATE_NO_WINDOW: u32 = 0x08000000;", script)
        self.assertIn("fn configure_background_process(", script)
        self.assertIn(".creation_flags(CREATE_NO_WINDOW);", script)
        self.assertIn("configure_background_process(&mut process);", script)
        self.assertIn("fn run_install_command(command: &PlannedCommand)", script)
        install_command = script.split("fn run_install_command", 1)[1].split(
            "fn wants_parakeet_backend", 1
        )[0]
        self.assertIn("configure_background_process(&mut process);", install_command)

    def test_windows_shell_open_helpers_do_not_show_console_windows(self):
        ui = rust_ui_source()
        config = rust_config_source()

        ui_open_url = ui.split("fn open_url", 1)[1].split("#[cfg(test)]", 1)[0]
        config_open_path = config.split("fn open_path", 1)[1].split("#[cfg(test)]", 1)[0]
        for helper in (ui_open_url, config_open_path):
            self.assertIn('Command::new("cmd")', helper)
            self.assertIn('.args(["/C", "start", ""', helper)
            self.assertIn(".creation_flags(0x08000000)", helper)

    def test_rust_ui_does_not_spawn_shell_cleanup_before_starting_window(self):
        ui_script = rust_ui_source()
        runtime_script = Path("src/rust/runtime.rs").read_text(encoding="utf-8")

        ui_run = ui_script.split("pub fn run() -> Result<()>", 1)[1].split(
            "impl Default for WhisperDictateApp", 1
        )[0]
        self.assertNotIn("cleanup_stale_desktop_processes", ui_run)
        self.assertIn("eframe::run_native(", ui_run)
        self.assertIn("#[cfg(windows)]\npub fn cleanup_stale_desktop_processes()", runtime_script)
        self.assertIn("#[cfg(not(windows))]\npub fn cleanup_stale_desktop_processes() {}", runtime_script)
        self.assertIn("fn cleanup_stale_desktop_processes_windows() -> Result<()>", runtime_script)
        self.assertIn("fn stale_process_cleanup_script(", runtime_script)
        self.assertIn("$cleanupPid = $PID", runtime_script)
        self.assertIn("$_.ProcessId -ne $cleanupPid", runtime_script)
        self.assertIn("fn windows_shell_program() -> &'static str", runtime_script)
        self.assertIn('"pwsh.exe"', runtime_script)
        self.assertIn("$_.ExecutablePath -eq $exe", runtime_script)
        self.assertIn('$_.CommandLine -like "*whisper_dictate.runtime*"', runtime_script)
        self.assertIn('$_.CommandLine -like "*$root*"', runtime_script)
        runtime_without_tests = runtime_script.split("#[cfg(test)]", 1)[0]
        self.assertNotIn("Stop-Process -Name python", runtime_without_tests)

    def test_rust_runtime_log_expands_to_available_width(self):
        script = rust_ui_source()
        runtime_tab = script.split("fn runtime_tab", 1)[1].split("fn settings_panel", 1)[0]

        self.assertIn("egui::ScrollArea::vertical()", runtime_tab)
        self.assertIn('.id_salt("runtime_log_scroll")', runtime_tab)
        # The log card fills the remaining height (computed at its own position)
        # so it ends at the panel's content bottom instead of overflowing below
        # the window; no fixed min-height that can exceed a short window.
        self.assertIn("let log_height = (ui.available_height() - (RUNTIME_LOG_TOP_MARGIN + 10.0)).max(0.0);", runtime_tab)
        self.assertNotIn("RUNTIME_LOG_VERTICAL_CHROME", script)
        self.assertNotIn("RUNTIME_LOG_MIN_HEIGHT", script)
        self.assertIn(".max_height(log_height)", runtime_tab)
        self.assertIn(".stick_to_bottom(true)", runtime_tab)
        self.assertIn("ui.set_min_height(log_height);", runtime_tab)
        self.assertIn("ui.set_min_width(ui.available_width());", runtime_tab)
        self.assertNotIn("ui.set_min_size(egui::vec2(ui.available_width(), log_height));", runtime_tab)
        self.assertIn("const RUNTIME_LOG_TOP_MARGIN: f32 = 16.0;", script)
        self.assertIn("const RUNTIME_LOG_CONTENT_TOP_PADDING: f32 = 10.0;", script)
        self.assertIn("const RUNTIME_LOG_CONTENT_BOTTOM_PADDING: f32 = 14.0;", script)
        self.assertIn("runtime_log_frame(palette).show(ui, |ui|", runtime_tab)
        self.assertIn("top: RUNTIME_LOG_TOP_MARGIN,", script)
        self.assertIn("ui.add_space(RUNTIME_LOG_CONTENT_TOP_PADDING);", runtime_tab)
        self.assertIn(
            "egui::vec2(ui.available_width(), RUNTIME_LOG_CONTENT_BOTTOM_PADDING)",
            runtime_tab,
        )
        self.assertIn('if card.title.trim().is_empty() {', runtime_tab)
        self.assertIn("bottom.scroll_to_me(Some(egui::Align::BOTTOM));", runtime_tab)
        self.assertIn("self.live_dictation_panel(ui, palette);", runtime_tab)
        self.assertNotIn("dashboard_side_panel", runtime_tab)
        self.assertNotIn("self.session_panel(ui, palette)", runtime_tab)
        self.assertNotIn("self.output_logging_panel(ui, palette)", runtime_tab)
        self.assertNotIn('.id_salt("runtime_dashboard_side_scroll")', runtime_tab)
        self.assertNotIn('.id_salt("runtime_dashboard_scroll")', runtime_tab)
        runtime_header = runtime_tab.split("ui.add_space(12.0);", 1)[0]
        log_toolbar = runtime_tab.split("ui.add_space(12.0);", 1)[1].split("ui.add_space(10.0);", 1)[0]
        self.assertIn(
            "fn listening_gauge(&self, ui: &mut egui::Ui, palette: UiPalette, max_width: f32)",
            runtime_tab,
        )
        self.assertIn("let mic_width = (ui.available_width() - 10.0).clamp(0.0, MIC_INDICATOR_MAX_WIDTH);", runtime_header)
        self.assertIn("self.listening_gauge(ui, palette, mic_width);", runtime_header)
        self.assertNotIn("self.listening_gauge", log_toolbar)
        self.assertIn("MIC_INDICATOR_MIN_WIDTH", script)
        self.assertIn("fn mic_label_char_budget(width: f32) -> usize", script)
        self.assertIn("fn audio_device_label(value: &str, max_chars: usize) -> String", script)
        self.assertIn("level_gauge(ui, palette, level, active, gauge_width)", runtime_tab)
        self.assertIn("fn level_gauge(", script)
        self.assertIn("fn gauge_color_for_position(", script)
        self.assertIn("audio_capture_active: bool", script)
        self.assertIn("audio_capture_opening: bool", script)
        self.assertIn("audio_meter_level: f32", script)
        self.assertIn("audio_meter_raw_dbfs: Option<f32>", script)
        self.assertIn("active_audio_device: String", script)
        self.assertIn(
            "let active = self.audio_capture_active && self.runtime_state == RuntimeState::Running;",
            script,
        )
        self.assertIn('} else if self.audio_capture_opening {', script)
        self.assertIn('"Opening"', script)
        self.assertIn("self.update_worker_audio(event)", script)
        self.assertIn("fn worker_event_f32(payload: &serde_json::Value, key: &str) -> Option<f32>", script)
        self.assertIn("audio_meter_level(self.audio_meter_level, self.runtime_state, active)", script)
        self.assertIn("fn audio_capture_active_for_worker_state(state: &str) -> Option<bool>", script)
        self.assertIn("fn worker_status_log_line(event: &WorkerEvent) -> Option<String>", script)
        self.assertIn('"startup_ms"', script)
        self.assertIn('"first_audio"', script)
        self.assertIn('"opening" | "ready" | "transcribing" | "loading_model" | "failed" => Some(false)', script)
        self.assertNotIn("fn latest_audio_peak", script)
        self.assertNotIn("parse_metric_f32(line", script)
        self.assertNotIn("fn audio_input_panel", script)
        self.assertNotIn("self.audio_input_panel(ui, palette)", runtime_tab)
        self.assertNotIn("let active = self.runtime_state != RuntimeState::Stopped;", script)

    def test_rust_output_tab_owns_logging_and_session_controls(self):
        script = rust_ui_source()
        output_tab = script.split("pub(in crate::ui) fn output_tab", 1)[1].split(
            "pub(in crate::ui) fn post_processing_tab", 1
        )[0]

        self.assertIn('section_label(ui, "Log view", palette);', output_tab)
        self.assertIn("self.log_mode_selector(ui, palette);", output_tab)
        self.assertIn("self.settings.ui_log_view = mode.id().to_owned();", script)
        self.assertIn("let ui_language = self.settings.ui_language.clone();", output_tab)
        self.assertIn("theme_toggle(ui, &mut self.settings.ui_theme, palette, &ui_language);", output_tab)
        self.assertIn("language_toggle(ui, &mut self.settings.ui_language, palette);", output_tab)
        self.assertIn("self.session_panel(ui, palette);", output_tab)
        self.assertIn("UiTextKey::UiTheme", output_tab)
        self.assertIn("UiTextKey::UiLanguage", output_tab)

    def test_rust_runtime_log_can_be_copied(self):
        script = rust_ui_source()

        self.assertIn("icons::ICON_COPY_ALL", script)
        self.assertIn("UiTextKey::Copy", script)
        self.assertIn("ui.ctx().copy_text(self.visible_runtime_log())", script)
        self.assertIn("LogViewMode::Minimal => final_output_text(log)", script)
        self.assertIn("fn final_output_text(log: &str) -> String", script)
        self.assertIn("fn from_raw(raw: &str) -> Self", script)
        self.assertIn('LogViewMode::Diagnostic => "diagnostic"', script)
        self.assertIn(".filter_map(extract_inject_preview)", script)
        runtime_tab = script.split("fn runtime_tab", 1)[1].split("fn settings_panel", 1)[0]
        self.assertIn("egui::Label::new(", runtime_tab)
        self.assertIn("let visible_log = self.visible_runtime_log();", runtime_tab)
        self.assertIn("egui::RichText::new(visible_log)", runtime_tab)
        self.assertIn(".monospace()", runtime_tab)
        self.assertIn(".color(palette.text)", runtime_tab)
        self.assertIn(".selectable(true)", runtime_tab)
        self.assertNotIn("TextEdit::multiline", runtime_tab)
        self.assertNotIn(".interactive(false)", runtime_tab)

    def test_rust_runtime_tab_can_clear_log_without_stopping_runtime(self):
        script = rust_ui_source()

        self.assertIn("icons::ICON_DELETE", script)
        self.assertIn("UiTextKey::Clear", script)
        self.assertIn("self.runtime_log.clear();", script)
        self.assertIn("self.runtime_log_scroll_to_bottom = true;", script)

    def test_rust_ui_shows_version_in_title_and_top_bar(self):
        script = rust_ui_source()
        icon = Path("src/rust/ui/icon.rs").read_text(encoding="utf-8")
        cargo = Path("src/rust/Cargo.toml").read_text(encoding="utf-8")

        self.assertIn('&format!("whisper-dictate {}", runtime::version())', script)
        self.assertIn("app_version: runtime::version()", script)
        self.assertIn('egui_material_icons = "0.2.0"', cargo)
        self.assertIn("egui_material_icons::initialize(&cc.egui_ctx)", script)
        self.assertIn("fn icon_text(", script)
        self.assertIn("fn icon(self) -> &'static str", script)
        self.assertIn("egui_material_icons::icons::ICON_MIC", script)
        self.assertIn("egui_material_icons::icons::ICON_OUTPUT", script)
        self.assertIn('egui::RichText::new("whisper-dictate")', script)
        self.assertIn('egui::SidePanel::left("primary_navigation")', script)
        self.assertIn('egui::TopBottomPanel::top("runtime_status")', script)
        update_impl = script.split("impl eframe::App for WhisperDictateApp", 1)[1].split(
            "impl WhisperDictateApp", 1
        )[0]
        self.assertNotIn("runtime::version()", update_impl)
        self.assertIn(".strong()", script)
        self.assertIn('.resizable(false)', script)
        self.assertIn("fn nav_button(", script)
        self.assertIn(".with_icon(app_icon())", script)
        self.assertIn("fn app_icon() -> egui::IconData", icon)

    def test_rust_ui_uses_soft_sidebar_and_settings_footer_layout(self):
        script = rust_ui_source()
        update_impl = script.split("impl eframe::App for WhisperDictateApp", 1)[1].split(
            "impl WhisperDictateApp", 1
        )[0]
        settings_panel = script.split("pub(in crate::ui) fn settings_panel", 1)[1].split(
            "pub(in crate::ui) fn core_tab", 1
        )[0]

        self.assertIn('egui::SidePanel::left("primary_navigation")', update_impl)
        self.assertIn("paint_sidebar_bridge(ctx, palette, &self.settings.ui_text_scale);", update_impl)
        self.assertIn("fn paint_sidebar_bridge(", script)
        self.assertIn("ctx.layer_painter(egui::LayerId::background())", script)
        self.assertIn(".show_separator_line(false)", update_impl)
        navigation_frame = update_impl.split('egui::SidePanel::left("primary_navigation")', 1)[
            1
        ].split(".show(ctx, |ui| self.sidebar(ui, palette))", 1)[0]
        self.assertIn(".stroke(egui::Stroke::NONE)", navigation_frame)
        self.assertIn("const SETTINGS_FOOTER_HEIGHT: f32 = 72.0;", script)
        self.assertIn("pub(in crate::ui) const EDGE_MARGIN: f32 = 12.0;", script)
        self.assertNotIn("SETTINGS_MESSAGES_BOTTOM_GAP", script)
        # Status messages moved out of the footer into the global bottom bar.
        self.assertIn('egui::TopBottomPanel::bottom("status_message_bar")', update_impl)
        self.assertNotIn("SETTINGS_MESSAGES_MAX_HEIGHT", script)
        self.assertNotIn("fn settings_messages", script)
        self.assertIn('ui.label(egui::RichText::new("Config:").color(palette.text_muted));', settings_panel)
        self.assertIn("let config_chars = ((ui.available_width() / 8.0).floor() as usize).clamp(38, 92);", settings_panel)
        self.assertIn("egui::RichText::new(compact_label(&self.config_path, config_chars))", settings_panel)
        self.assertIn(".monospace()", settings_panel)
        self.assertIn(".on_hover_text(&self.config_path)", settings_panel)
        self.assertNotIn('ui.label(format!("Config: {}", self.config_path));', settings_panel)

    def test_rust_runtime_controls_live_in_fixed_top_status_bar(self):
        script = rust_ui_source()
        update_impl = script.split("impl eframe::App for WhisperDictateApp", 1)[1].split(
            "impl WhisperDictateApp", 1
        )[0]
        # global_controls now lives in ui/tabs/shell.rs; scope to that file so the
        # assertNotIns aren't polluted by other tabs in the concatenated source.
        controls = (
            Path("src/rust/ui/tabs/shell.rs")
            .read_text(encoding="utf-8")
            .split("fn global_controls", 1)[1]
        )

        self.assertIn("self.sidebar(ui, palette)", update_impl)
        self.assertIn("self.top_status_bar(ui, palette)", update_impl)
        self.assertLess(
            update_impl.index("self.top_status_bar(ui, palette)"),
            update_impl.index("egui::CentralPanel::default()"),
        )
        self.assertIn("top_status_bar_height(&self.settings.ui_text_scale)", update_impl)
        self.assertIn('egui::SidePanel::left("primary_navigation")', update_impl)
        self.assertIn('egui::TopBottomPanel::top("runtime_status")', update_impl)
        self.assertNotIn(".exact_height(76.0)", update_impl)
        self.assertNotIn("ui.horizontal_centered", update_impl)
        self.assertIn("let is_stopped = self.runtime_state == RuntimeState::Stopped;", controls)
        self.assertIn("let is_active = !is_stopped;", controls)
        self.assertIn("icons::ICON_PLAY_ARROW", controls)
        self.assertIn("icons::ICON_STOP", controls)
        self.assertIn("UiTextKey::Start", controls)
        self.assertIn("UiTextKey::Stop", controls)
        self.assertNotIn('egui::Button::new(icon_text(icons::ICON_RESTART_ALT, "Restart"))', controls)
        self.assertNotIn("self.restart_runtime();", controls)
        self.assertIn("fn top_status_controls_width() -> f32", script)
        self.assertIn("let controls_width = top_status_controls_width();", script)
        self.assertIn("fn status_card_wide(", script)
        self.assertIn("status_card_wide(", script)
        self.assertIn("ui.set_min_width(min_width);", script)
        self.assertIn(".on_hover_text(value);", script)
        self.assertIn("icons::ICON_REFRESH", script)
        self.assertIn("UiTextKey::ReloadConfig", script)
        self.assertNotIn("Reload settings", controls)
        self.assertIn("UiTextKey::InstallRepair", script)
        self.assertNotIn("UiTextKey::InstallRepair", controls)
        self.assertIn("fn top_status_bar_height(raw_scale: &str) -> f32", script)
        self.assertIn("fn sidebar_width(raw_scale: &str) -> f32", script)

