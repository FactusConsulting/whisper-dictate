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


class WindowsRustUiSettingsRegressionTests(unittest.TestCase):
    def test_rust_ui_has_cloud_provider_dropdown_and_key_storage(self):
        script = rust_ui_source()
        api_keys = Path("src/rust/ui/api_keys.rs").read_text(encoding="utf-8")

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
        # Field renders at its natural height; the reveal button is sized to that
        # height so the eye icon stays inline with the field.
        self.assertIn(".desired_width(input_width)", script)
        self.assertIn("eye_icon_button(ui, is_revealed, field.rect.height())", script)
        self.assertIn("fn save_stt_api_key_now(&mut self)", script)
        # `pub(in crate::ui)` visibility pushes this signature past rustfmt's width,
        # so it now wraps `&mut self` onto its own line; match the fn name + receiver
        # without assuming they stay on the same source line.
        self.assertIn("fn persist_cloud_provider_selection(", script)
        self.assertIn("&mut self,\n    ) -> Result<Option<std::path::PathBuf>>", script)
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
        api_keys = Path("src/rust/ui/api_keys.rs").read_text(encoding="utf-8")
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
                "src/rust/ui/api_key_store_tests.rs",
                "src/rust/ui/cloud_settings_tests.rs",
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
        self.assertIn("pub(in crate::ui) const EDGE_MARGIN: f32 = 12.0;", script)
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
        self.assertIn("ui.set_min_height(inner_height);", script)
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
        api_keys = Path("src/rust/ui/api_keys.rs").read_text(encoding="utf-8")

        self.assertIn('const POST_API_KEY_ENV: &str = "VOICEPI_POST_API_KEY"', api_keys)
        self.assertIn("enum PostProvider", api_keys)
        self.assertIn('"Post API key"', script)
        self.assertIn("Tab::Post => self.settings_panel(ui, Self::post_processing_tab)", script)
        self.assertIn("pub(in crate::ui) fn post_processing_tab(&mut self, ui: &mut egui::Ui)", script)
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
        cargo = Path("src/rust/Cargo.toml").read_text(encoding="utf-8")

        self.assertIn('keyring = { version = "3.6"', cargo)
        self.assertIn('"windows-native"', cargo)
        self.assertIn('"apple-native"', cargo)
        self.assertIn('"linux-native-sync-persistent"', cargo)
        self.assertIn('"crypto-rust"', cargo)

    def test_rust_ui_has_cloud_api_test_and_local_viewers(self):
        ui = rust_ui_source()
        lib = Path("src/rust/lib.rs").read_text(encoding="utf-8")
        cargo = Path("src/rust/Cargo.toml").read_text(encoding="utf-8")

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
        # `inline_help` and `apply_ui_theme` no longer share a file after the ui.rs
        # decomposition (widgets vs theme submodules), so read inline_help from the
        # widgets module where it now lives — it is the final fn in that file.
        widgets = Path("src/rust/ui/widgets.rs").read_text(encoding="utf-8")
        inline_help = widgets.split("fn inline_help", 1)[1]
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
        config = rust_config_source()

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

