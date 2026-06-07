from helpers import (
    Path,
    unittest,
)

def rust_ui_source():
    paths = [
        "src/rust/whisper-dictate-app/src/ui.rs",
        "src/rust/whisper-dictate-app/src/ui/tabs.rs",
        "src/rust/whisper-dictate-app/src/ui/api_keys.rs",
        "src/rust/whisper-dictate-app/src/ui/icon.rs",
    ]
    return "\n".join(Path(path).read_text(encoding="utf-8") for path in paths)

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
        script = Path("src/rust/whisper-dictate-app/src/main.rs").read_text(encoding="utf-8")

        self.assertIn(
            '#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]',
            script,
        )

    def test_rust_background_processes_hide_windows_console(self):
        script = Path("src/rust/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

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
        config = Path("src/rust/whisper-dictate-app/src/config.rs").read_text(encoding="utf-8")

        ui_open_url = ui.split("fn open_url", 1)[1].split("#[cfg(test)]", 1)[0]
        config_open_path = config.split("fn open_path", 1)[1].split("#[cfg(test)]", 1)[0]
        for helper in (ui_open_url, config_open_path):
            self.assertIn('Command::new("cmd")', helper)
            self.assertIn('.args(["/C", "start", ""', helper)
            self.assertIn(".creation_flags(0x08000000)", helper)

    def test_rust_ui_does_not_spawn_shell_cleanup_before_starting_window(self):
        ui_script = rust_ui_source()
        runtime_script = Path("src/rust/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

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
        self.assertIn("const RUNTIME_LOG_VERTICAL_CHROME: f32 = 112.0;", script)
        self.assertIn("const RUNTIME_LOG_MIN_HEIGHT: f32 = 300.0;", script)
        self.assertIn("let log_height = (height - RUNTIME_LOG_VERTICAL_CHROME).max(RUNTIME_LOG_MIN_HEIGHT);", runtime_tab)
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
        self.assertIn("self.live_dictation_panel(ui, palette, height);", runtime_tab)
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
        self.assertIn("self.update_worker_audio(&event);", script)
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
        output_tab = script.split("pub(super) fn output_tab", 1)[1].split(
            "pub(super) fn post_processing_tab", 1
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
        self.assertIn("egui::RichText::new(&visible_log)", runtime_tab)
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
        icon = Path("src/rust/whisper-dictate-app/src/ui/icon.rs").read_text(encoding="utf-8")
        cargo = Path("src/rust/whisper-dictate-app/Cargo.toml").read_text(encoding="utf-8")

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
        settings_panel = script.split("pub(super) fn settings_panel", 1)[1].split(
            "pub(super) fn core_tab", 1
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
        self.assertIn("const SETTINGS_FOOTER_HEIGHT: f32 = 264.0;", script)
        self.assertIn("const SETTINGS_MESSAGES_TOP_GAP: f32 = 14.0;", script)
        self.assertIn("const SETTINGS_MESSAGES_BOTTOM_GAP: f32 = 20.0;", script)
        self.assertIn("const SETTINGS_MESSAGES_MAX_HEIGHT: f32 = 88.0;", script)
        self.assertIn("ui.add_space(SETTINGS_MESSAGES_TOP_GAP);", settings_panel)
        self.assertIn("ui.add_space(SETTINGS_MESSAGES_BOTTOM_GAP);", settings_panel)
        self.assertIn("ui.set_min_height(112.0);", settings_panel)
        self.assertIn("ui.add_space(8.0);", settings_panel)
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
        controls = script.split("pub(super) fn global_controls", 1)[1].split(
            "pub(super) fn runtime_tab", 1
        )[0]

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

    def test_rust_ui_has_cloud_provider_dropdown_and_key_storage(self):
        script = rust_ui_source()
        api_keys = Path("src/rust/whisper-dictate-app/src/ui/api_keys.rs").read_text(encoding="utf-8")

        self.assertIn('GROQ_STT_BASE_URL: &str = "https://api.groq.com/openai/v1"', api_keys)
        self.assertIn('GROQ_STT_MODEL: &str = "whisper-large-v3-turbo"', api_keys)
        self.assertIn('OPENAI_STT_BASE_URL: &str = "https://api.openai.com/v1"', api_keys)
        self.assertIn("enum CloudProvider", api_keys)
        self.assertIn("const GROQ_STT_MODELS: &[&str]", script)
        self.assertIn("const OPENAI_STT_MODELS: &[&str]", script)
        self.assertIn("const WHISPER_MODELS: &[&str]", script)
        self.assertIn('"distil-whisper-large-v3-en"', script)
        self.assertIn('"gpt-4o-mini-transcribe"', script)
        self.assertIn("const STT_BACKEND_OPTIONS: &[(&str, &str)]", script)
        self.assertIn('("openai", "Cloud STT (Groq/OpenAI)")', script)
        self.assertIn("const CLOUD_PROVIDER_OPTIONS: &[(&str, &str)]", script)
        self.assertIn('"Speech engine",', script)
        self.assertIn("combo_help_labeled(", script)
        self.assertIn("combo_enabled_labeled(", script)
        self.assertIn('"Cloud STT provider",', script)
        self.assertIn('"Cloud STT model",', script)
        self.assertIn("provider.model_options()", script)
        self.assertIn('"gpt-4o-transcribe"', script)
        self.assertIn('"whisper-1"', script)
        self.assertIn("const PARAKEET_MODELS: &[&str]", script)
        self.assertIn('"nvidia/parakeet-tdt-0.6b-v3"', script)
        self.assertIn('"nvidia/parakeet-tdt-1.1b"', script)
        self.assertIn('"nvidia/parakeet-tdt-0.6b-v2"', script)
        self.assertIn('"Parakeet model",', script)
        self.assertIn("PARAKEET_MODELS", script)
        self.assertIn('GROQ_KEYS_URL: &str = "https://console.groq.com/keys"', api_keys)
        self.assertIn('OPENAI_KEYS_URL: &str = "https://platform.openai.com/api-keys"', api_keys)
        self.assertIn('"Cloud STT API key"', script)
        self.assertIn('"Save API key"', script)
        self.assertIn("const PASSWORD_CONTROL_WIDTH: f32 = 360.0;", script)
        self.assertIn("const EYE_BUTTON_WIDTH: f32 = 26.0;", script)
        self.assertIn("ui.set_width(PASSWORD_CONTROL_WIDTH);", script)
        self.assertIn("ui.add_sized(", script)
        self.assertIn("egui::vec2(input_width, 22.0)", script)
        self.assertIn("fn save_stt_api_key_now(&mut self)", script)
        self.assertIn("fn persist_cloud_provider_selection(&mut self)", script)
        self.assertIn("fn ensure_stt_api_key_loaded_for_runtime(&mut self)", script)
        self.assertIn("fn cloud_stt_missing_api_key(&self) -> bool", script)
        self.assertIn("fn save_stt_api_key_if_changed(", script)
        self.assertIn("keyring::Entry::new", api_keys)
        self.assertIn("STT_API_KEY_ENV", api_keys)
        self.assertIn("fn has_unsaved_settings(&self) -> bool", script)
        self.assertIn("icons::ICON_SAVE", script)
        self.assertIn("UiTextKey::SaveSettingsDirty", script)
        self.assertIn(".add_enabled(is_dirty, save_button)", script)
        self.assertIn('"Unsaved changes"', script)
        self.assertIn("icons::ICON_REFRESH", script)
        self.assertIn("UiTextKey::ReloadConfig", script)
        self.assertNotIn('ui.button("Clear API key").clicked()', script)

    def test_rust_ui_uses_same_api_key_loader_on_start_and_reload(self):
        script = rust_ui_source()
        api_keys = Path("src/rust/whisper-dictate-app/src/ui/api_keys.rs").read_text(encoding="utf-8")
        default_impl = script.split("impl Default for WhisperDictateApp", 1)[1].split(
            "struct BackgroundTaskResult", 1
        )[0]
        reload_impl = script.split("fn reload_stt_api_key", 1)[1].split(
            "fn save_stt_api_key_if_changed", 1
        )[0]

        self.assertIn("load_stt_api_key_state(provider)", default_impl)
        self.assertIn("load_stt_api_key_state(provider)", reload_impl)
        self.assertIn("fn load_stt_api_key_state(provider: CloudProvider)", api_keys)
        self.assertIn("Loaded {} API key from environment. Use Save API key to store it.", api_keys)
        ui_tests = "\n".join(
            Path(path).read_text(encoding="utf-8")
            for path in (
                "src/rust/whisper-dictate-app/src/ui/api_key_store_tests.rs",
                "src/rust/whisper-dictate-app/src/ui/cloud_settings_tests.rs",
            )
        )
        self.assertIn("environment_api_keys_do_not_make_settings_dirty_at_startup", ui_tests)
        self.assertIn("edited_api_key_still_makes_settings_dirty", ui_tests)
        self.assertIn("successful_keyring_save_keeps_file_fallback", ui_tests)
        self.assertIn("saving_api_key_persists_selected_cloud_provider_settings", ui_tests)

    def test_api_check_results_are_visible_next_to_buttons_and_in_runtime_log(self):
        script = rust_ui_source()

        self.assertIn("fn set_api_check_status(&mut self, label: &str, message: &str)", script)
        self.assertIn('self.stt_api_key_status = message.to_owned()', script)
        self.assertIn('self.post_api_key_status = message.to_owned()', script)
        self.assertIn('format!("[OK] {} passed: {detail}", result.label)', script)
        self.assertIn('format!("[ERROR] {} failed to run: {error}", result.label)', script)
        self.assertIn("fn status_label(ui: &mut egui::Ui, text: &str, palette: UiPalette)", script)
        self.assertIn('text.starts_with("[OK]")', script)
        self.assertIn('text.starts_with("[ERROR]")', script)
        self.assertIn("fn settings_messages(&self, ui: &mut egui::Ui)", script)
        self.assertIn("UiTextKey::Messages", script)
        self.assertIn("UiTextKey::NoMessages", script)
        self.assertIn("Tab::Speech if !self.stt_api_key_status.trim().is_empty()", script)
        self.assertIn("Tab::Post if !self.post_api_key_status.trim().is_empty()", script)
        self.assertIn("let palette = ui_palette(&self.settings.ui_theme);", script)
        self.assertIn("status_label(ui, message, palette);", script)

    def test_rust_settings_pages_scroll_above_fixed_footer(self):
        script = rust_ui_source()
        settings_panel = script.split("fn settings_panel", 1)[1].split("fn settings_messages", 1)[0]

        self.assertIn("const SETTINGS_FOOTER_HEIGHT: f32 = 264.0;", script)
        self.assertIn("const SETTINGS_FOOTER_CHROME_HEIGHT: f32 = 18.0;", script)
        self.assertIn("const SETTINGS_MESSAGES_TOP_GAP: f32 = 14.0;", script)
        self.assertIn("const SETTINGS_MESSAGES_BOTTOM_GAP: f32 = 20.0;", script)
        self.assertIn("const SETTINGS_MESSAGES_MAX_HEIGHT: f32 = 88.0;", script)
        self.assertIn("let footer_height = SETTINGS_FOOTER_HEIGHT;", settings_panel)
        self.assertIn("ui.available_height() - footer_height - SETTINGS_FOOTER_CHROME_HEIGHT", settings_panel)
        self.assertIn("egui::Layout::top_down(egui::Align::LEFT)", settings_panel)
        self.assertIn("self.settings_messages(ui);", settings_panel)
        self.assertIn("self.settings_actions(ui);", settings_panel)
        self.assertIn("egui::ScrollArea::vertical()", settings_panel)
        self.assertIn('.id_salt(format!("settings_body_{:?}", self.selected_tab))', settings_panel)
        self.assertIn(".max_height(body_height)", settings_panel)
        self.assertIn("body(self, ui);", settings_panel)
        self.assertIn("fn settings_actions(&mut self, ui: &mut egui::Ui)", script)
        self.assertIn("ui.horizontal_wrapped(|ui|", script)
        self.assertIn("ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);", script)
        self.assertIn('ui.label(egui::RichText::new("Config:").color(palette.text_muted));', script)
        self.assertIn("egui::RichText::new(compact_label(&self.config_path, config_chars))", script)
        self.assertIn(".on_hover_text(&self.config_path)", script)
        self.assertNotIn('ui.label(format!("Config: {}", self.config_path));', script)
        self.assertIn("ui.add_space(SETTINGS_MESSAGES_TOP_GAP);", settings_panel)
        self.assertIn("ui.add_space(SETTINGS_MESSAGES_BOTTOM_GAP);", settings_panel)
        self.assertIn("ui.set_min_height(112.0);", script)
        self.assertIn('.id_salt(format!("settings_messages_{:?}", self.selected_tab))', script)
        self.assertIn(".max_height(SETTINGS_MESSAGES_MAX_HEIGHT)", script)
        self.assertIn("egui::Label::new(rich_text).wrap()", script)
        self.assertIn("fn panel_frame(palette: UiPalette) -> egui::Frame", script)
        self.assertIn(".rounding(egui::Rounding::same(PANEL_RADIUS as f32))", script)
        self.assertIn("egui::Margin::symmetric(16.0, 14.0)", script)

    def test_rust_core_ui_groups_backend_specific_models_and_help(self):
        script = rust_ui_source()

        self.assertIn("enum SttBackendMode", script)
        self.assertIn("Local Whisper", script)
        self.assertIn("Local NVIDIA Parakeet", script)
        self.assertIn("Cloud STT", script)
        self.assertIn("backend == SttBackendMode::Whisper", script)
        self.assertIn("backend == SttBackendMode::Parakeet", script)
        self.assertIn("backend == SttBackendMode::Cloud", script)
        self.assertIn("backend != SttBackendMode::Cloud", script)
        self.assertIn("fn help_badge(", script)
        self.assertIn('small_button("?")', script)
        self.assertIn("label_with_help_enabled(", script)

    def test_rust_output_ui_supports_groq_postprocess_models(self):
        script = rust_ui_source()
        api_keys = Path("src/rust/whisper-dictate-app/src/ui/api_keys.rs").read_text(encoding="utf-8")

        self.assertIn('const POST_API_KEY_ENV: &str = "VOICEPI_POST_API_KEY"', api_keys)
        self.assertIn("enum PostProvider", api_keys)
        self.assertIn('"Post API key"', script)
        self.assertIn("Tab::Post => self.settings_panel(ui, Self::post_processing_tab)", script)
        self.assertIn("pub(super) fn post_processing_tab(&mut self, ui: &mut egui::Ui)", script)
        self.assertIn('settings_grid("post_processing_settings")', script)
        self.assertIn("fn settings_grid(id: &'static str) -> egui::Grid", script)
        self.assertIn(".spacing(egui::vec2(20.0, 10.0))", script)
        self.assertIn('"Save post API key"', script)
        self.assertIn("fn save_post_api_key_now(&mut self)", script)
        self.assertIn("fn load_post_api_key_state(", api_keys)
        self.assertIn("fn load_post_api_key_from_env(provider: PostProvider)", api_keys)
        self.assertIn("fn save_post_api_key(", api_keys)
        self.assertIn("provider: PostProvider,", api_keys)
        self.assertIn("Result<SecretSaveReport>", api_keys)
        self.assertIn("self.reload_post_api_key();", script)
        self.assertIn("const GROQ_POST_MODELS: &[(&str, &str)]", script)
        self.assertIn('GROQ_POST_MODEL: &str = "llama-3.3-70b-versatile"', api_keys)
        self.assertIn('"llama-3.1-8b-instant"', script)
        self.assertIn('"llama-3.3-70b-versatile"', script)
        self.assertIn('"llama-3.3-70b-versatile - recommended Danish final check"', script)
        self.assertIn('"qwen/qwen3-32b - strong multilingual, use hidden reasoning"', script)
        self.assertIn('"openai/gpt-oss-20b - fast quality/cost candidate"', script)
        self.assertIn('"llama-4-scout-17b - preview, not preferred for Danish"', script)
        self.assertIn('"qwen/qwen3-32b"', script)
        self.assertIn('"openai/gpt-oss-20b"', script)
        self.assertIn('"groq/compound-mini"', script)
        self.assertIn("combo_help_labeled(", script)
        self.assertIn("fn labeled_options_contain(", script)
        self.assertIn("const POST_PROCESSOR_OPTIONS: &[(&str, &str)]", script)
        self.assertIn('("groq", "Groq")', script)
        self.assertIn('("openai", "OpenAI")', script)
        self.assertIn('matches!(self.settings.post_processor.as_str(), "openai" | "groq")', script)
        self.assertIn("let post_key = self.post_api_key_input.trim();", script)
        self.assertIn("self.stt_api_key_input.trim()", script)
        self.assertIn("fn normalize_postprocessor_settings(&mut self)", script)
        self.assertIn("GROQ_STT_BASE_URL.to_owned()", script)
        self.assertIn("Optional second text pass after speech recognition", script)
        self.assertIn("Controls what the post processor is allowed to do", script)
        self.assertIn("raw bypasses post-processing", script)
        self.assertIn('"Test post API"', script)
        self.assertIn("fn run_post_api_check(&mut self)", script)
        self.assertIn("response.on_hover_text(help)", script)
        self.assertIn('"Quit key"', script)

    def test_rust_ui_keyring_uses_native_platform_backends(self):
        cargo = Path("src/rust/whisper-dictate-app/Cargo.toml").read_text(encoding="utf-8")

        self.assertIn('keyring = { version = "3.6"', cargo)
        self.assertIn('"windows-native"', cargo)
        self.assertIn('"apple-native"', cargo)
        self.assertIn('"linux-native-sync-persistent"', cargo)
        self.assertIn('"crypto-rust"', cargo)

    def test_rust_ui_has_cloud_api_test_and_local_viewers(self):
        ui = rust_ui_source()
        lib = Path("src/rust/whisper-dictate-app/src/lib.rs").read_text(encoding="utf-8")
        cargo = Path("src/rust/whisper-dictate-app/Cargo.toml").read_text(encoding="utf-8")

        self.assertIn("pub mod cloud_api;", lib)
        self.assertIn("pub mod telemetry;", lib)
        self.assertIn('ureq = { version = "2.12"', cargo)
        self.assertIn('"Test cloud API"', ui)
        self.assertIn("fn run_cloud_api_check(&mut self)", ui)
        self.assertIn("check_cloud_api(&check)", ui)
        self.assertIn('"Test post API"', ui)
        self.assertIn("check_post_api(&check)", ui)
        self.assertIn('"Preview history"', ui)
        self.assertIn('"Preview metrics"', ui)
        self.assertIn("telemetry::preview_jsonl", ui)

    def test_rust_settings_tabs_have_visible_help_badges(self):
        script = rust_ui_source()

        self.assertIn("fn help_badge(", script)
        self.assertIn('small_button("?")', script)
        self.assertIn("fn label_with_help(", script)
        self.assertIn("fn label_with_help_enabled(", script)
        self.assertIn("fn checkbox_help(", script)
        self.assertIn("label_with_help(ui, label, help)", script)
        self.assertIn("label_with_help_enabled(ui, enabled, label, help)", script)
        self.assertIn("fn grid_help_row(", script)
        self.assertIn("fn inline_help(", script)
        self.assertIn("fn apply_ui_theme(", script)
        self.assertIn("DEFAULT_UI_TEXT_SCALE", script)
        self.assertIn("style.text_styles = text_styles", script)
        self.assertNotIn(".small().color(ui.visuals().weak_text_color())", script)
        self.assertIn("data.insert_persisted(id, show_help)", script)
        self.assertIn("response.on_hover_text(help)", script)
        inline_help = script.split("fn inline_help", 1)[1].split("fn apply_ui_theme", 1)[0]
        self.assertIn("egui::Label::new", inline_help)
        self.assertIn(".wrap()", inline_help)
        self.assertNotIn("ui.label(egui::RichText::new(help)", inline_help)
        for label in (
            "STT backend",
            "Whisper model",
            "Parakeet model",
            "Cloud STT model",
            "Linux keyboard layout",
            "Beam size",
            "VAD speech pad ms",
            "Audio ducking",
            "Audio ducking level",
            "Initial prompt",
            "Dictionary path",
            "Dictionary enabled",
            "Inject mode",
            "JSON stdout",
            "Cloud redaction",
            "Redaction terms",
            "UI theme",
            "UI text scale",
            "Profiles JSON",
        ):
            self.assertIn(label, script)

    def test_rust_ui_uses_switchable_accented_themes(self):
        script = rust_ui_source()
        config = Path("src/rust/whisper-dictate-app/src/config.rs").read_text(encoding="utf-8")

        self.assertIn("const UI_ACCENT_BLUE: egui::Color32", script)
        self.assertIn("egui::Color32::from_rgb(125, 211, 252)", script)
        self.assertIn("const UI_ACCENT_DARK: egui::Color32", script)
        self.assertIn("const UI_LIGHT_ACCENT_BLUE: egui::Color32", script)
        self.assertIn("const UI_LIGHT_HEADER_BG: egui::Color32", script)
        self.assertIn("enum UiThemeMode", script)
        self.assertIn('match raw {\n            "light" => Self::Light,', script)
        self.assertIn("struct UiPalette", script)
        self.assertIn("fn ui_palette(raw_theme: &str) -> UiPalette", script)
        self.assertIn("fn apply_ui_theme(", script)
        self.assertIn("fn themed_visuals(theme: UiThemeMode, palette: UiPalette) -> egui::Visuals", script)
        self.assertIn("egui::Visuals::dark()", script)
        self.assertIn("egui::Visuals::light()", script)
        self.assertIn("visuals.panel_fill = palette.panel_bg", script)
        self.assertIn("visuals.selection.bg_fill = palette.selection_bg", script)
        self.assertIn("style.visuals = themed_visuals(theme, palette)", script)
        self.assertIn("fn nav_button(", script)
        self.assertIn("palette.accent_dark", script)
        self.assertIn("egui::Button::new(text).fill(fill).stroke(stroke)", script)
        self.assertIn("palette.header_bg", script)
        self.assertIn("fn runtime_status_badge(", script)
        self.assertIn("UiTextKey::Status", script)
        self.assertIn("UiTextKey::UiTheme", script)
        self.assertIn("fn theme_toggle(", script)
        self.assertIn("UiTextKey::Dark", script)
        self.assertIn("UiTextKey::Light", script)
        self.assertIn('pub ui_theme: String', config)
        self.assertIn('pub ui_language: String', config)
        self.assertIn('pub ui_log_view: String', config)
        self.assertIn('ui_theme: "dark".to_owned()', config)
        self.assertIn('ui_language: "en".to_owned()', config)
        self.assertIn('ui_log_view: "minimal".to_owned()', config)
        self.assertIn('validate_choice("ui_theme", &self.ui_theme, &["dark", "light"])', config)
        self.assertIn('validate_choice("ui_language", &self.ui_language, &["en", "da"])', config)
        self.assertIn('"ui_log_view",', config)
        self.assertIn('&["minimal", "diagnostic", "debug"]', config)
        self.assertIn('set_string(object, "ui_theme", &self.ui_theme)', config)
        self.assertIn('set_string(object, "ui_language", &self.ui_language)', config)
        self.assertIn('set_string(object, "ui_log_view", &self.ui_log_view)', config)
        restart_keys = config.split("const RESTART_KEYS", 1)[1].split("];", 1)[0]
        self.assertNotIn('"ui_theme"', restart_keys)
        self.assertNotIn('"ui_language"', restart_keys)
        self.assertNotIn('"ui_log_view"', restart_keys)
        self.assertIn("enum UiLanguageMode", script)
        self.assertIn("enum UiTextKey", script)
        self.assertIn('UiTextKey::Speech => "Tale"', script)
        self.assertIn("fn language_toggle(", script)
        self.assertIn('"UI language"', script)
        self.assertIn('("", "Auto")', script)
        self.assertIn('("da", "Danish")', script)
        self.assertIn('if !cfg!(windows) {', script)
        self.assertIn('"Linux keyboard layout"', script)
        self.assertIn("Local runtime", script)
        self.assertIn("Dictation controls", script)
        self.assertIn("Applies to local and cloud speech engines.", script)
        self.assertIn('"Wayland ydotool/XKB layout used for direct text injection on Linux.', script)

    def test_config_maps_audio_ducking_and_cloud_redaction(self):
        config = Path("src/python/whisper_dictate/settings_schema.json").read_text(encoding="utf-8")
        rust_config = Path("src/rust/whisper-dictate-app/src/config.rs").read_text(encoding="utf-8")
        ui = rust_ui_source()

        for token in (
            "VOICEPI_AUDIO_DUCKING",
            "VOICEPI_AUDIO_DUCKING_LEVEL",
            "VOICEPI_VAD_SPEECH_PAD_MS",
            "VOICEPI_POST_REDACT",
            "VOICEPI_POST_REDACT_TERMS",
        ):
            self.assertIn(token, config)
        for key in (
            "audio_ducking",
            "audio_ducking_level",
            "vad_speech_pad_ms",
            "post_redact",
            "post_redact_terms",
        ):
            self.assertIn(key, rust_config)
            self.assertIn(key, ui)

    def test_rust_cli_has_explicit_ubuntu_setup_command(self):
        cli = Path("src/rust/whisper-dictate-app/src/cli.rs").read_text(encoding="utf-8")
        main = Path("src/rust/whisper-dictate-app/src/main.rs").read_text(encoding="utf-8")
        runtime = Path("src/rust/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

        self.assertIn("SetupUbuntu", cli)
        self.assertIn('["whisper-dictate", "setup-ubuntu"]', cli)
        self.assertIn("Command::SetupUbuntu => runtime::setup_ubuntu()", main)
        self.assertIn("pub fn setup_ubuntu() -> Result<()>", runtime)
        self.assertIn("fn ubuntu_setup_script_path(root: &Path) -> PathBuf", runtime)
        self.assertIn('join("packaging")', runtime)
        self.assertIn('join("linux")', runtime)
        self.assertIn('join("ubuntu26.04")', runtime)
        self.assertIn('join("setup.sh")', runtime)
        self.assertIn('env("VOICEPI_RUST_OWNS_DESKTOP", "1")', runtime)
        self.assertIn("fn install_linux_desktop_entries() -> Result<()>", runtime)
        self.assertIn(
            "fn linux_desktop_entry(autostart: bool, exec: &str, icon: &Path) -> String",
            runtime,
        )
        self.assertIn("fn start_linux_ui_detached() -> Result<()>", runtime)

    def test_ubuntu_setup_creates_launcher_autostart_and_starts_rust_ui(self):
        script = Path("packaging/linux/ubuntu26.04/setup.sh").read_text(encoding="utf-8")
        runtime = Path("src/rust/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

        self.assertIn('VOICEPI_RUST_OWNS_DESKTOP', script)
        self.assertIn("fn linux_desktop_exec_command() -> String", runtime)
        self.assertIn('format!("{} ui", desktop_exec_token(&exe))', runtime)
        self.assertIn('Name=Whisper Dictate', runtime)
        self.assertIn('.local/share/applications', runtime)
        self.assertIn('.config/autostart', runtime)
        self.assertIn('gtk-launch', runtime)
        self.assertIn('setsid', runtime)
        self.assertIn('Terminal-runtime: whisper-dictate run --key shift_r+ctrl_r --lang da', script)
        self.assertNotIn('Exec=whisper-dictate --key shift_r+ctrl_r --lang da', script)

    def test_ubuntu_setup_uses_bash_conditionals_for_reliability(self):
        script = Path("packaging/linux/ubuntu26.04/setup.sh").read_text(encoding="utf-8")

        self.assertIn("[[ ! -f /usr/local/bin/gcc-12 ]]", script)
        self.assertIn('[[ -n "$GCC" ]]', script)
        self.assertIn('[[ -f "$UDEV_FILE" ]]', script)
        self.assertIn('[[ "${VOICEPI_RUST_OWNS_DESKTOP:-}" = "1" ]]', script)
        self.assertNotRegex(script, r"(?m)^\s*if\s+\[\s+!?-")
        self.assertNotRegex(script, r"(?m)^\s*if\s+\[\s+-n")

    def test_windows_docs_use_rust_terminal_entrypoint(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")
        technical = Path("docs/TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("runs the Rust UI and starts the Python worker hidden underneath it", readme)
        self.assertIn("whisper-dictate run --key ctrl_r --lang da", readme)
        self.assertIn(r"whisper-dictate.exe run --key ctrl_r --lang da --device cuda", readme)
        self.assertIn("whisper-dictate.exe\" run --key ctrl_r --lang da --model large-v3 --device cuda", config)
        self.assertIn(r"whisper-dictate.exe run --key ctrl_r --lang da", config)
        self.assertIn("Rust UI is the installer Start-menu", technical)
        self.assertIn("no compatibility script is installed", technical)
        self.assertNotIn("whisper-dictate Terminal", readme)
        self.assertNotIn("whisper-dictate Debug Terminal", readme)
        self.assertNotIn("Current primary path is the installed PySide/PowerShell UI", technical)

    def test_docs_describe_groq_as_explicit_opt_in_without_storing_keys(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")

        for doc in (readme, config):
            self.assertIn("https://api.groq.com/openai/v1", doc)
            self.assertIn("whisper-large-v3-turbo", doc)
            self.assertIn("GROQ_API_KEY", doc)
            self.assertIn("VOICEPI_POST_API_KEY", doc)
        self.assertIn("Cloud STT provider", config)
        self.assertIn("Post processor", readme)
        self.assertIn("Post API key", readme)
        self.assertIn("OS credential store", readme)

    def test_docs_describe_one_command_ubuntu_setup_and_launcher_start(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")

        for doc in (readme, config):
            self.assertIn("whisper-dictate setup-ubuntu", doc)
            self.assertIn("Whisper Dictate", doc)
            self.assertIn("whisper-dictate ui", doc)
        self.assertIn("Then press **Start** in the Runtime tab", readme)

    def test_installer_uses_whisper_dictate_icon_and_searchable_ui_name(self):
        with open("packaging/windows/inno/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r"SetupIconFile=..\..\..\assets\whisper-dictate.ico", script)
        self.assertIn(r'Source: "..\..\..\assets\whisper-dictate.ico"', script)
        self.assertIn(r'IconFilename: "{app}\whisper-dictate.ico"', script)
        self.assertNotIn(r"Legacy Settings UI", script)
        self.assertNotIn(r"\Settings UI", script)

    def test_windows_icon_is_multiresolution_and_has_source_logo(self):
        icon = Path("assets/whisper-dictate.ico").read_bytes()
        svg = Path("assets/whisper-dictate-logo.svg").read_text(encoding="utf-8")

        self.assertGreater(len(icon), 10_000)
        self.assertEqual(int.from_bytes(icon[0:2], "little"), 0)
        self.assertEqual(int.from_bytes(icon[2:4], "little"), 1)
        self.assertEqual(int.from_bytes(icon[4:6], "little"), 7)
        sizes = set()
        for i in range(7):
            entry = 6 + i * 16
            sizes.add(256 if icon[entry] == 0 else icon[entry])
            image_offset = int.from_bytes(icon[entry + 12:entry + 16], "little")
            self.assertEqual(icon[image_offset:image_offset + 8], b"\x89PNG\r\n\x1a\n")
        self.assertEqual(sizes, {16, 24, 32, 48, 64, 128, 256})
        self.assertIn("viewBox=\"0 0 256 256\"", svg)
        self.assertIn("linearGradient", svg)
        self.assertIn("fill=\"#FFFFFF\"", svg)

    def test_rust_windows_binary_embeds_application_icon_resource(self):
        cargo = Path("src/rust/whisper-dictate-app/Cargo.toml").read_text(encoding="utf-8")
        build = Path("src/rust/whisper-dictate-app/build.rs").read_text(encoding="utf-8")

        self.assertIn("winresource", cargo)
        self.assertIn("CARGO_CFG_TARGET_OS", build)
        self.assertIn('"windows"', build)
        self.assertIn("../../../assets/whisper-dictate.ico", build)
        self.assertIn("resource.compile()", build)

    def test_github_docs_show_logo(self):
        readme = Path("README.md").read_text(encoding="utf-8")

        self.assertIn('src="assets/whisper-dictate-logo.svg"', readme)
        self.assertIn("<h1 align=\"center\">whisper-dictate</h1>", readme)

    def test_installer_creates_desktop_ui_shortcut(self):
        with open("packaging/windows/inno/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r'Name: "{userdesktop}\whisper-dictate"', script)
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"', script)
        self.assertIn(r'Parameters: "ui"', script)

    def test_installer_packages_rust_ui_as_primary_desktop_entry(self):
        with open("packaging/windows/inno/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r'Source: "..\..\..\target\release\whisper-dictate.exe"', script)
        self.assertIn(
            r'Name: "{userprograms}\whisper-dictate\whisper-dictate";    Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"',
            script,
        )
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"; Description: "Launch whisper-dictate now"', script)

    def test_windows_installer_workflows_build_rust_ui_before_inno(self):
        for path in (".github/workflows/release.yml", ".github/workflows/windows-installer.yml"):
            workflow = Path(path).read_text(encoding="utf-8")
            rust_build = workflow.index("cargo build --manifest-path src/rust/Cargo.toml --target-dir target --release -p whisper-dictate-app")
            installer_build = workflow.index("Build installers")
            self.assertLess(rust_build, installer_build)
            self.assertIn("src/rust/Cargo.toml src/rust/Cargo.lock src/rust/", workflow)

        script = Path("scripts/windows/build-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("cargo build --manifest-path (Join-Path $root 'src\\rust\\Cargo.toml') --target-dir (Join-Path $root 'target') --release -p whisper-dictate-app", script)
        self.assertIn("cargo build failed", script)

    def test_local_windows_installer_defaults_to_semver_build_metadata(self):
        script = Path("scripts/windows/build-installer.ps1").read_text(encoding="utf-8")
        installer = Path("packaging/windows/inno/whisper-dictate.iss").read_text(encoding="utf-8")
        readme = Path("README.md").read_text(encoding="utf-8")

        self.assertIn("function Get-CrateVersion", script)
        self.assertIn("src\\rust\\whisper-dictate-app\\Cargo.toml", script)
        self.assertIn("function Get-LocalBuildMetadata", script)
        self.assertIn('Version = "$(Get-CrateVersion)+$(Get-LocalBuildMetadata)"', script)
        self.assertNotIn("function Get-LatestReleaseVersion", script)
        self.assertNotIn("git -C $root tag --list", script)
        self.assertIn("Get-Date -Format 'yyyyMMddHHmmss'", script)
        self.assertIn('dirty = ".dirty"', script)
        self.assertIn("function Get-VersionInfoVersion", script)
        self.assertIn('/DVERSION_INFO=$versionInfo', script)
        self.assertIn("#ifndef VERSION_INFO", installer)
        self.assertIn("VersionInfoVersion={#VERSION_INFO}", installer)
        self.assertIn("<version>+local.<timestamp>.g<sha>.dirty", readme)
        self.assertIn("<major>.<minor>.<patch>.1", readme)

    def test_windows_zip_packages_are_built_on_windows_with_rust_exe(self):
        for path in (".github/workflows/release.yml", ".github/workflows/windows-installer.yml"):
            workflow = Path(path).read_text(encoding="utf-8")

            self.assertIn("Build Windows ZIP packages", workflow)
            self.assertIn("whisper-dictate-windows-$version.zip", workflow)
            self.assertIn("whisper-dictate-windows-setup-$version.exe", workflow)
            self.assertIn("Copy-Item target\\release\\whisper-dictate.exe", workflow)
            self.assertIn("Copy-Item assets\\whisper-dictate.ico", workflow)
            self.assertNotIn('Copy-Item requirements-cpu.txt (Join-Path $bundle "requirements.txt")', workflow)
            self.assertNotIn('"requirements.txt"', workflow)
            self.assertIn("Copy-Item requirements $bundle -Recurse", workflow)
            self.assertIn("Output/*.exe Output/*.zip Output/*.nupkg sha256sums.txt", workflow)

        script = Path("scripts/windows/build-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("Building unified Windows portable ZIP version $Version", script)
        self.assertIn("whisper-dictate-windows-$Version.zip", script)
        self.assertIn("whisper-dictate-windows-setup-$Version.exe", script)
        self.assertIn("target\\release\\whisper-dictate.exe", script)
        self.assertIn("assets\\whisper-dictate.ico", script)
        self.assertNotIn("requirements.txt", script)
        self.assertIn("Join-Path $root 'requirements'", script)
        self.assertIn("Compress-Archive", script)

    def test_docs_describe_windows_zip_and_installer_outputs(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        agents = Path("AGENTS.md").read_text(encoding="utf-8")
        technical = Path("docs/TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("portable Windows ZIP bundle", readme)
        self.assertIn("installer and portable ZIP are written to `Output\\`", readme)
        self.assertIn("Output\\*.exe` and `Output\\*.zip", agents)
        self.assertIn("Output\\*.exe` and `Output\\*.zip", technical)

    def test_project_history_is_not_maintained_as_docs(self):
        workflow = Path(".github/workflows/release.yml").read_text(encoding="utf-8")

        self.assertFalse(Path("docs/CHANGELOG.md").exists())
        self.assertFalse(Path("docs/RELEASE_NOTES.md").exists())
        self.assertIn("git log --no-merges", workflow)
        self.assertNotIn("docs/RELEASE_NOTES.md", workflow)

    def test_runtime_reconfigures_windows_streams_to_utf8(self):
        with open("src/python/whisper_dictate/runtime.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn('reconfigure(encoding="utf-8", errors="replace")', script)

    def test_runtime_has_parakeet_min_duration_and_backend_metrics(self):
        with open("src/python/whisper_dictate/runtime.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("self.parakeet_min_seconds", script)
        self.assertIn("too short for Parakeet", script)
        self.assertIn("stt_backend=self.stt_backend", script)

    def test_runtime_has_live_release_tail_padding(self):
        with open("src/python/whisper_dictate/runtime.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("self.release_tail_ms", script)
        self.assertIn('after.get("release_tail_ms", "200")', script)
        self.assertIn("time.sleep(tail_s)", script)

    def test_cli_debug_prints_parakeet_min_seconds(self):
        with open("src/python/whisper_dictate/vp_cli.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("parakeet_min_s", script)
        self.assertIn("VOICEPI_PARAKEET_MIN_SECONDS", script)
        self.assertIn("release_tail_ms", script)
        self.assertIn("VOICEPI_RELEASE_TAIL_MS", script)
