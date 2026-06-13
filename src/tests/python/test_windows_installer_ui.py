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
        # A silent install (winget/Chocolatey) must not hang on the auto-answered
        # retry prompt — it fails fast with the reason instead.
        self.assertIn("WizardSilent()", prepare)
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
        self.assertIn("top: RUNTIME_LOG_TOP_MARGIN as i8,", script)
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
        # Assert the full inactive arm INSIDE the function body — both the state
        # list and that it maps to Some(false) — so an accidental behavior
        # change (e.g. flipping the return) cannot slip past the guard.
        capture_active_fn = script.split(
            "fn audio_capture_active_for_worker_state", 1
        )[1].split("\n}", 1)[0]
        inactive_arm = capture_active_fn.split(
            '"opening" | "ready" | "transcribing" | "loading_model" | "failed" | "no_text"\n        | "capture_lost"', 1
        )
        self.assertEqual(len(inactive_arm), 2, "inactive state list missing")
        self.assertIn("Some(false)", inactive_arm[1].split("=>", 2)[1])
        self.assertNotIn("fn latest_audio_peak", script)
        self.assertNotIn("parse_metric_f32(line", script)
        self.assertNotIn("fn audio_input_panel", script)
        self.assertNotIn("self.audio_input_panel(ui, palette)", runtime_tab)
        self.assertNotIn("let active = self.runtime_state != RuntimeState::Stopped;", script)

    def test_rust_output_tab_is_pure_speech_output(self):
        script = rust_ui_source()
        output_tab = script.split("pub(in crate::ui) fn output_tab", 1)[1].split(
            "pub(in crate::ui) fn post_processing_tab", 1
        )[0]

        # Speech-output controls stay on the Output tab.
        self.assertIn("self.session_panel(ui, palette);", output_tab)
        self.assertIn('"Inject mode"', output_tab)
        self.assertIn('"Format commands"', output_tab)
        self.assertIn('"Command hook"', output_tab)
        self.assertIn('"Command hook timeout ms"', output_tab)
        self.assertIn("&mut self.settings.history_enabled", output_tab)
        self.assertIn("&mut self.settings.local_only", output_tab)
        self.assertIn("self.preview_history();", output_tab)
        self.assertIn("self.open_history();", output_tab)

        # App-level chrome + machine-readable outputs MOVED to the System tab and
        # must no longer appear on Output. This encodes the new IA so a regression
        # that re-adds them to Output fails here.
        self.assertNotIn("theme_toggle(", output_tab)
        self.assertNotIn("language_toggle(", output_tab)
        self.assertNotIn("self.log_mode_selector(ui, palette);", output_tab)
        self.assertNotIn('"Dictation view"', output_tab)
        self.assertNotIn("UiTextKey::UiTheme", output_tab)
        self.assertNotIn("UiTextKey::UiLanguage", output_tab)
        self.assertNotIn("UiTextKey::DictationView", output_tab)
        self.assertNotIn("&mut self.settings.ui_text_scale", output_tab)
        self.assertNotIn("&mut self.settings.inject_json", output_tab)
        self.assertNotIn("&mut self.settings.metrics_jsonl", output_tab)
        self.assertNotIn("&mut self.settings.feedback_sounds", output_tab)
        self.assertNotIn("&mut self.settings.feedback_notify", output_tab)

    def test_rust_system_tab_owns_maintenance_and_app_settings(self):
        script = rust_ui_source()
        system_tab = script.split("pub(in crate::ui) fn system_tab", 1)[1].split(
            "fn open_config_folder", 1
        )[0]

        # The System tab exists and is wired into the tab enum + dispatch.
        self.assertIn("Tab::System", script)
        self.assertIn("Tab::System => self.settings_panel(ui, Self::system_tab)", script)
        self.assertIn("Tab::System => egui_material_icons::icons::ICON_SETTINGS", script)

        # Maintenance: the three sidebar actions moved here, same handlers.
        self.assertIn("self.reload_settings();", system_tab)
        self.assertIn("self.run_doctor();", system_tab)
        self.assertIn("self.run_install();", system_tab)
        self.assertIn("UiTextKey::ReloadConfig", system_tab)
        self.assertIn("UiTextKey::Doctor", system_tab)
        self.assertIn("UiTextKey::InstallRepair", system_tab)
        # The install/reload buttons keep the "another task running" guard.
        self.assertIn("let idle = self.background_task.is_none();", system_tab)
        # Config-file shortcut: hover EXPLAINS the action and shows the path,
        # click opens the folder via the console-guarded helper (not a
        # reimplemented shell call).
        self.assertIn("UiTextKey::ConfigFile", system_tab)
        self.assertIn("Opens the folder containing config.json.", system_tab)
        self.assertIn("self.config_path", system_tab)
        self.assertIn("self.open_config_folder();", system_tab)
        self.assertIn("config::open_existing_path(&folder)", script)

        # Appearance + Display + Feedback + Integration settings moved here.
        self.assertIn("theme_toggle(ui, &mut self.settings.ui_theme, palette, &ui_language);", system_tab)
        # UI language is an extensible dropdown now, not the old two-button toggle.
        self.assertIn('egui::ComboBox::from_id_salt("ui_language_select")', system_tab)
        self.assertIn("self.log_mode_selector(ui, palette);", system_tab)
        self.assertIn("&mut self.settings.ui_text_scale", system_tab)
        self.assertIn("&mut self.settings.feedback_sounds", system_tab)
        self.assertIn("&mut self.settings.feedback_notify", system_tab)
        self.assertIn("&mut self.settings.inject_json", system_tab)
        self.assertIn("&mut self.settings.metrics_jsonl", system_tab)
        self.assertIn("UiTextKey::SystemMaintenance", system_tab)
        self.assertIn("UiTextKey::SystemAppearance", system_tab)
        self.assertIn("UiTextKey::SystemDisplay", system_tab)
        self.assertIn("UiTextKey::SystemFeedback", system_tab)
        self.assertIn("UiTextKey::SystemIntegration", system_tab)

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
        self.assertIn('egui_material_icons = "0.6.0"', cargo)
        self.assertIn("egui_material_icons::initialize(&cc.egui_ctx)", script)
        self.assertIn("fn icon_text(", script)
        self.assertIn("fn icon(self) -> &'static str", script)
        self.assertIn("egui_material_icons::icons::ICON_MIC", script)
        self.assertIn("egui_material_icons::icons::ICON_OUTPUT", script)
        self.assertIn('egui::RichText::new("whisper-dictate")', script)
        self.assertIn('egui::Panel::left("primary_navigation")', script)
        self.assertIn('egui::Panel::top("runtime_status")', script)
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

        self.assertIn('egui::Panel::left("primary_navigation")', update_impl)
        self.assertIn("paint_sidebar_bridge(ctx, palette, &self.settings.ui_text_scale);", update_impl)
        self.assertIn("fn paint_sidebar_bridge(", script)
        self.assertIn("ctx.layer_painter(egui::LayerId::background())", script)
        self.assertIn(".show_separator_line(false)", update_impl)
        navigation_frame = update_impl.split('egui::Panel::left("primary_navigation")', 1)[
            1
        ].split(".show_inside(ui, |ui| self.sidebar(ui, palette))", 1)[0]
        self.assertIn(".stroke(egui::Stroke::NONE)", navigation_frame)
        # The footer slimmed to a single Reset-page action row after Reload config
        # + the config path moved to the System tab.
        self.assertIn("const SETTINGS_FOOTER_HEIGHT: f32 = 40.0;", script)
        self.assertIn("pub(in crate::ui) const EDGE_MARGIN: f32 = 12.0;", script)
        self.assertNotIn("SETTINGS_MESSAGES_BOTTOM_GAP", script)
        # Status messages moved out of the footer into the global bottom bar.
        self.assertIn('egui::Panel::bottom("status_message_bar")', update_impl)
        self.assertNotIn("SETTINGS_MESSAGES_MAX_HEIGHT", script)
        self.assertNotIn("fn settings_messages", script)
        # The per-page footer keeps only Reset page; the Reload config button and
        # the "Config:" path row are gone (the path now lives on the System tab).
        self.assertIn("UiTextKey::ResetPage", settings_panel)
        self.assertNotIn('ui.label(egui::RichText::new("Config:").color(palette.text_muted));', settings_panel)
        self.assertNotIn("compact_label(&self.config_path, config_chars)", settings_panel)
        self.assertNotIn("self.reload_settings();", settings_panel)
        self.assertNotIn('ui.label(format!("Config: {}", self.config_path));', settings_panel)

    def test_rust_sidebar_bottom_block_is_slim(self):
        # The sidebar bottom block keeps only the PTT chord, Save settings, and
        # the version. Reload config / Doctor / Install-Repair moved to System.
        sidebar = (
            Path("src/rust/ui/tabs/shell.rs")
            .read_text(encoding="utf-8")
            .split("fn sidebar", 1)[1]
            .split("fn status_message_bar", 1)[0]
        )
        # Kept affordances.
        self.assertIn("UiTextKey::SaveSettings", sidebar)
        self.assertIn("format_push_to_talk_keys(&self.settings.key)", sidebar)
        self.assertIn('format!("v{}", self.app_version)', sidebar)
        self.assertIn("self.save_settings();", sidebar)
        # Maintenance actions are NOT in the sidebar anymore.
        self.assertNotIn("self.run_install();", sidebar)
        self.assertNotIn("self.run_doctor();", sidebar)
        self.assertNotIn("self.reload_settings();", sidebar)
        # The tab ScrollArea sits inside the bottom_up block, whose layout the
        # ScrollArea content INHERITS — without an explicit top_down wrapper the
        # tabs render reversed (System on top; 1.8.10 regression).
        self.assertIn('.id_salt("sidebar_tab_scroll")', sidebar)
        self.assertIn(
            "ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {",
            sidebar,
        )
        self.assertNotIn("UiTextKey::InstallRepair", sidebar)
        self.assertNotIn("UiTextKey::Doctor", sidebar)
        self.assertNotIn("UiTextKey::ReloadConfig", sidebar)

    def test_rust_top_status_bar_shows_post_processing_indicator(self):
        shell = Path("src/rust/ui/tabs/shell.rs").read_text(encoding="utf-8")
        # The pure top-bar layout + post-indicator helpers were split out of
        # shell.rs into their own module so the render code stays small.
        layout = Path("src/rust/ui/tabs/top_status_layout.rs").read_text(encoding="utf-8")

        # The indicator is rendered inside the left status-card region (so it
        # shares the clipping budget), before the right-pinned controls.
        top_bar = shell.split("fn top_status_bar", 1)[1].split("fn global_controls", 1)[0]
        self.assertIn("self.post_indicator(ui, palette);", top_bar)
        # On/off decision + label + hover are pure, testable functions mirroring
        # the worker gate (processor != none/empty AND mode != raw, where the
        # worker normalizes an EMPTY mode to raw — so unset mode reads as off).
        self.assertIn("fn post_processing_enabled(processor: &str, mode: &str) -> bool", layout)
        self.assertIn(
            '!processor.is_empty() && processor != "none" && !mode.is_empty() && mode != "raw"',
            layout,
        )
        self.assertIn("fn post_indicator_label(", layout)
        self.assertIn("fn post_indicator_hover(", layout)
        self.assertIn("UiTextKey::PostOn", layout)
        self.assertIn("UiTextKey::PostOff", layout)

    def test_rust_nav_button_inactive_tabs_look_clickable(self):
        theme = Path("src/rust/ui/theme.rs").read_text(encoding="utf-8")
        nav = theme.split("fn nav_button", 1)[1].split("fn icon_text", 1)[0]
        # Inactive tabs get a visible surface + soft border (not transparent /
        # no-stroke) plus a pointing-hand cursor so they read as buttons.
        self.assertIn("egui::CursorIcon::PointingHand", nav)
        self.assertIn("palette.surface_bg", nav)
        self.assertIn("egui::Stroke::new(0.8, palette.border_soft)", nav)
        self.assertNotIn("egui::Color32::TRANSPARENT", nav)
        self.assertNotIn("egui::Stroke::NONE", nav)

    def test_rust_compact_mic_label_derives_budget_from_width(self):
        compact = Path("src/rust/ui/tabs/compact.rs").read_text(encoding="utf-8")
        # The fixed character budget is gone; the budget now derives from the
        # available label width so widening the strip reveals the full name.
        self.assertNotIn("COMPACT_DEVICE_LABEL_CHARS", compact)
        self.assertIn("fn compact_mic_label_char_budget(width: f32) -> usize", compact)
        self.assertIn("let device_chars = compact_mic_label_char_budget(label_width);", compact)
        self.assertIn("ui.available_width()", compact.split("fn compact_mic", 1)[1])

    def test_rust_runtime_controls_live_in_fixed_top_status_bar(self):
        script = rust_ui_source()
        shell = Path("src/rust/ui/tabs/shell.rs").read_text(encoding="utf-8")
        # The pure fit/width budget now lives in its own module.
        layout = Path("src/rust/ui/tabs/top_status_layout.rs").read_text(encoding="utf-8")
        update_impl = script.split("impl eframe::App for WhisperDictateApp", 1)[1].split(
            "impl WhisperDictateApp", 1
        )[0]
        # global_controls now lives in ui/tabs/shell.rs; scope to that file so the
        # assertNotIns aren't polluted by other tabs in the concatenated source.
        controls = shell.split("fn global_controls", 1)[1]

        # Compact mode is a session-only early branch: the runtime/background
        # polls must run BEFORE it so dictation keeps flowing in the strip, and
        # the branch renders ONLY a CentralPanel that calls compact_panel, then
        # returns before the full-window chrome. Split at the early return so the
        # full-window ordering assertions below see the real (post-branch) layout.
        self.assertIn("if self.compact_mode {", update_impl)
        self.assertLess(
            update_impl.index("self.poll_runtime();"),
            update_impl.index("if self.compact_mode {"),
        )
        self.assertLess(
            update_impl.index("self.poll_background_task();"),
            update_impl.index("if self.compact_mode {"),
        )
        compact_block, full_layout = update_impl.split("if self.compact_mode {", 1)[
            1
        ].split("return;", 1)
        self.assertIn("egui::CentralPanel::default()", compact_block)
        self.assertIn("self.compact_panel(ui, palette)", compact_block)
        # The full layout must NOT be re-entered after the compact branch returns:
        # sidebar / status bars only exist in the post-return portion.
        self.assertNotIn("self.sidebar(ui, palette)", compact_block)
        self.assertNotIn("self.top_status_bar(ui, palette)", compact_block)

        self.assertIn("self.sidebar(ui, palette)", full_layout)
        self.assertIn("self.top_status_bar(ui, palette)", full_layout)
        self.assertLess(
            full_layout.index("self.top_status_bar(ui, palette)"),
            full_layout.index("egui::CentralPanel::default()"),
        )
        self.assertIn("top_status_bar_height(&self.settings.ui_text_scale)", full_layout)
        self.assertIn('egui::Panel::left("primary_navigation")', update_impl)
        self.assertIn('egui::Panel::top("runtime_status")', update_impl)
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
        # The live item_spacing gap between the two regions is reserved too —
        # it scales with the UI text scale (Copilot finding on PR #167).
        self.assertIn(
            "let controls_width = top_status_controls_width() + ui.spacing().item_spacing.x;",
            script,
        )
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
        # Priority-drop budget: pure function (now in top_status_layout.rs) +
        # call site in top_status_bar.
        self.assertIn(
            "pub(in crate::ui) fn top_status_cards_fit(",
            layout,
        )
        # Signature may be wrapped by rustfmt across lines — check the pieces.
        self.assertIn("fn top_status_cards_fit(", layout)
        self.assertIn("left_width: f32,", layout)
        self.assertIn("card_widths: &[f32],", layout)
        self.assertIn("spacing: f32,", layout)
        top_bar = shell.split("fn top_status_bar", 1)[1].split("fn global_controls", 1)[0]
        self.assertIn("top_status_cards_fit(", top_bar)
        self.assertIn("let fit_count = top_status_cards_fit(", top_bar)
        # The budget MUST use each card's TRUE OUTER width (inner min-width +
        # Frame margin + stroke), not the bare inner set_min_width value, or
        # cards overflow the left region and clip mid-card.
        self.assertIn("fn status_card_outer_width(raw_scale: &str) -> f32", script)
        self.assertIn("fn status_card_wide_outer_width(raw_scale: &str) -> f32", script)
        self.assertIn("fn post_indicator_outer_width(raw_scale: &str) -> f32", script)
        self.assertIn("status_card_outer_width(", top_bar)
        self.assertIn("status_card_wide_outer_width(", top_bar)
        self.assertIn("post_indicator_outer_width(", top_bar)
        # Single source of truth: the Frame margin/stroke consts are shared
        # between the card/pill render and the outer-width helpers.
        self.assertIn("STATUS_CARD_H_MARGIN", script)
        self.assertIn("POST_PILL_H_MARGIN", script)
        self.assertIn("CARD_STROKE", script)
        # Cards are rendered in priority order, highest first; skips use fit_count.
        self.assertIn("fit_count > 1", top_bar)
        self.assertIn("fit_count > 2", top_bar)
        self.assertIn("fit_count > 3", top_bar)

    def test_rust_top_status_cards_are_flat_readouts_not_buttons(self):
        shell = Path("src/rust/ui/tabs/shell.rs").read_text(encoding="utf-8")
        theme = Path("src/rust/ui/theme.rs").read_text(encoding="utf-8")
        # Scope to the two readout renderers: the post_indicator pill (between
        # top_status_bar and global_controls) and the status_card_sized Frame
        # body (scoped to its own function so unrelated additions later in
        # shell.rs cannot pollute the assertions). Concatenate both so the style
        # assertions cover the pill AND the cards but never the actual buttons
        # in global_controls.
        pill = shell.split("fn post_indicator", 1)[1].split("fn global_controls", 1)[0]
        # Scope status_card_sized to just its own function body: split at the
        # fn header, then cut off at the next fn definition so later helpers
        # (fn runtime_state_color, etc.) can't accidentally satisfy an assertIn.
        cards = shell.split("fn status_card_sized", 1)[1].split("\nfn ", 1)[0]
        readouts = pill + cards

        # Status cards + post pill must read as flat READOUTS, not the raised,
        # clickable Start/Stop/compact buttons next to them:
        #  - a recessed readout_bg tint (darker than panel_bg in dark mode so
        #    cards read as a faint recess; buttons use surface_bg), NOT surface_bg
        #  - no border tell (CARD_STROKE is 0.0)
        #  - a barely-rounded READOUT_RADIUS corner, not the buttons' radius
        self.assertNotIn("palette.surface_bg", readouts)
        self.assertIn("palette.readout_bg", readouts)
        self.assertIn("READOUT_RADIUS", readouts)
        self.assertNotIn("PANEL_RADIUS", readouts)
        self.assertNotIn("PILL_RADIUS", readouts)
        self.assertIn("pub(in crate::ui) const READOUT_RADIUS: u8 = 4;", theme)
        self.assertIn("pub(in crate::ui) const CARD_STROKE: f32 = 0.0;", theme)

        # The label no longer uses a hardcoded 12.0 size — it goes through the
        # centralized Small text style so it scales with the UI like the value.
        self.assertNotIn(".size(12.0)", cards)
        self.assertIn("egui::TextStyle::Small", cards)
        # Value still shows full text on hover.
        self.assertIn(".on_hover_text(value);", cards)

        # The buttons themselves STAY buttons: surface/accent fills live in
        # global_controls, untouched.
        controls = shell.split("fn global_controls", 1)[1].split("fn status_card", 1)[0]
        self.assertIn("egui::Button::new", controls)

    def test_rust_top_status_panel_height_derives_from_card_content(self):
        theme = Path("src/rust/ui/theme.rs").read_text(encoding="utf-8")
        # The fixed 64px panel height is gone; the height is derived from the real
        # two-line card content so the rounded card bottom is never clipped at any
        # scale (the unscaled margins no longer fall behind the scaled text).
        self.assertNotIn("const TOP_STATUS_HEIGHT", theme)
        self.assertIn("fn status_card_height(raw_scale: &str) -> f32", theme)
        height = theme.split("fn top_status_bar_height", 1)[1].split("\n}", 1)[0]
        self.assertIn("status_card_height(raw_scale)", height)
        self.assertIn("TOP_PANEL_V_MARGIN", height)

    def test_rust_compact_mode_is_session_only_always_on_top_strip(self):
        script = rust_ui_source()
        compact = Path("src/rust/ui/tabs/compact.rs").read_text(encoding="utf-8")
        config = rust_config_source()
        controls = (
            Path("src/rust/ui/tabs/shell.rs")
            .read_text(encoding="utf-8")
            .split("fn global_controls", 1)[1]
        )

        # Session-only UI flag on the app state — never wired into config.
        self.assertIn("compact_mode: bool", script)
        self.assertNotIn("compact_mode", config)

        # The top status bar's controls expose an icon-only enter-compact toggle.
        self.assertIn("icons::ICON_PICTURE_IN_PICTURE_ALT", controls)
        self.assertIn("Compact mode — small always-on-top strip", controls)
        self.assertIn("self.set_compact_mode(ui.ctx(), true);", controls)

        # Toggling sends viewport commands as a pure, testable data list and the
        # method only flips state + replays them (no worker restart).
        self.assertIn(
            "fn compact_toggle_viewport_cmds(enter: bool) -> Vec<egui::ViewportCommand>",
            compact,
        )
        self.assertIn(
            "egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop)", compact
        )
        self.assertIn("egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal)", compact)
        self.assertIn("egui::ViewportCommand::InnerSize(COMPACT_INNER_SIZE.into())", compact)
        self.assertIn(
            "egui::ViewportCommand::MinInnerSize(COMPACT_MIN_INNER_SIZE.into())", compact
        )
        self.assertIn("egui::ViewportCommand::Decorations(true)", compact)
        self.assertIn("egui::ViewportCommand::InnerSize(FULL_INNER_SIZE.into())", compact)
        self.assertIn(
            "egui::ViewportCommand::MinInnerSize(FULL_MIN_INNER_SIZE.into())", compact
        )
        self.assertIn("ctx.send_viewport_cmd(cmd);", compact)
        self.assertNotIn("self.start_runtime()", compact.split("fn set_compact_mode", 1)[1].split("\n    }", 1)[0])
        self.assertNotIn("self.supervisor", compact)

        # The restored full-window floor matches run()'s launch floor exactly.
        self.assertIn("const FULL_MIN_INNER_SIZE: [f32; 2] = [1000.0, 640.0];", compact)
        self.assertIn("const FULL_INNER_SIZE: [f32; 2] = [1080.0, 760.0];", compact)
        self.assertIn(".with_min_inner_size([1000.0, 640.0])", script)
        self.assertIn(".with_inner_size([1080.0, 760.0])", script)

        # The compact panel reuses the shared Start/Stop lifecycle + level gauge
        # and an exit-compact button, plus the live pipeline progress line.
        self.assertIn("icons::ICON_OPEN_IN_FULL", compact)
        self.assertIn("Leave compact mode", compact)
        self.assertIn("self.set_compact_mode(ui.ctx(), false);", compact)
        self.assertIn("self.start_runtime();", compact)
        self.assertIn("self.stop_runtime();", compact)
        self.assertIn("level_gauge(ui, palette, level, active, gauge_width)", compact)
        self.assertIn("fn compact_stage_label(", compact)

