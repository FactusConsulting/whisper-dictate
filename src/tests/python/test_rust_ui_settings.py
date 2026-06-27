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
        # Wave 8 of #348 removed the Parakeet model picker and its const
        # together with the backend. Pin the inverse so a future contributor
        # who reintroduces a hard-coded NVIDIA model name here fails loudly.
        self.assertNotIn("const PARAKEET_MODELS:", script)
        self.assertNotIn("nvidia/parakeet-tdt-0.6b-v3", script)
        self.assertNotIn("nvidia/parakeet-tdt-1.1b", script)
        self.assertNotIn("nvidia/parakeet-tdt-0.6b-v2", script)
        self.assertNotIn('"Parakeet model",', script)
        self.assertNotIn(
            '("parakeet", "Local NVIDIA Parakeet")', script)
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
        self.assertIn("fn credential_entry(user: &str) -> Result<Entry>", api_keys)
        self.assertIn("Entry::new(CREDENTIAL_SERVICE, user)", api_keys)
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

    def test_api_check_results_surface_in_runtime_log_and_status_bar(self):
        script = rust_ui_source()

        self.assertIn("fn set_api_check_status(&mut self, label: &str, message: &str)", script)
        self.assertIn('self.stt_api_key_status = message.to_owned()', script)
        self.assertIn('self.post_api_key_status = message.to_owned()', script)
        self.assertIn('format!("[OK] {} passed: {detail}", result.label)', script)
        self.assertIn('format!("[ERROR] {} failed to run: {error}", result.label)', script)
        # API-key statuses surface in the global bottom message bar (not a
        # per-page Messages card any more).
        self.assertIn("fn status_message_bar(&self, ui: &mut egui::Ui, palette: UiPalette)", script)
        self.assertIn("fn status_bar_message(&self) -> String", script)
        self.assertIn("Tab::Speech if !self.stt_api_key_status.trim().is_empty()", script)
        self.assertIn("Tab::Post if !self.post_api_key_status.trim().is_empty()", script)

    def test_global_bottom_message_bar_replaces_per_page_messages_card(self):
        script = rust_ui_source()

        # A single thin status bar at the bottom on every tab, fed by the saved
        # state + latest message — no per-page Messages card, no sidebar badge.
        self.assertIn('egui::Panel::bottom("status_message_bar")', script)
        self.assertIn("self.status_message_bar(ui, palette)", script)
        self.assertIn("bottom_message_bar_height(&self.settings.ui_text_scale)", script)
        self.assertIn("fn bottom_message_bar_height(raw_scale: &str) -> f32", script)
        self.assertNotIn("fn settings_messages", script)
        self.assertNotIn("fn sidebar_save_state", script)
        self.assertNotIn('format!("settings_messages_{:?}", self.selected_tab)', script)

    def test_rust_settings_pages_scroll_above_fixed_footer(self):
        script = rust_ui_source()
        settings_panel = script.split("fn settings_panel", 1)[1].split("fn settings_actions", 1)[0]

        # The footer slimmed to a single Reset-page row after Reload config + the
        # config path moved to the System tab.
        self.assertIn("const SETTINGS_FOOTER_HEIGHT: f32 = 40.0;", script)
        self.assertIn("const SETTINGS_FOOTER_CHROME_HEIGHT: f32 = 18.0;", script)
        self.assertIn("pub(in crate::ui) const EDGE_MARGIN: f32 = 12.0;", script)
        self.assertIn("let footer_height = SETTINGS_FOOTER_HEIGHT;", settings_panel)
        self.assertIn("ui.available_height() - footer_height - SETTINGS_FOOTER_CHROME_HEIGHT", settings_panel)
        self.assertIn("self.settings_actions(ui);", settings_panel)
        self.assertIn("egui::ScrollArea::vertical()", settings_panel)
        self.assertIn('.id_salt(format!("settings_body_{:?}", self.selected_tab))', settings_panel)
        self.assertIn(".max_height(body_height)", settings_panel)
        self.assertIn("body(self, ui);", settings_panel)
        self.assertIn("fn settings_actions(&mut self, ui: &mut egui::Ui)", script)
        self.assertIn("ui.horizontal_wrapped(|ui|", script)
        self.assertIn("ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);", script)
        # The config path now lives on the System tab as a hover on the Config
        # file button, not as a per-page "Config:" footer row.
        actions = script.split("fn settings_actions", 1)[1].split("fn reset_current_tab_settings", 1)[0]
        self.assertNotIn('ui.label(egui::RichText::new("Config:").color(palette.text_muted));', actions)
        self.assertNotIn("compact_label(&self.config_path, config_chars)", actions)
        self.assertNotIn("self.reload_settings();", actions)
        self.assertIn("fn panel_frame(palette: UiPalette) -> egui::Frame", script)
        self.assertIn(".corner_radius(egui::CornerRadius::same(PANEL_RADIUS))", script)
        self.assertIn("egui::Margin::symmetric(16, 14)", script)

    def test_rust_core_ui_groups_backend_specific_models_and_help(self):
        # Wave 8 of #348 collapsed SttBackendMode to (Whisper, Cloud); the
        # Parakeet variant + its "Local NVIDIA Parakeet" picker no longer
        # ship.
        script = rust_ui_source()

        self.assertIn("enum SttBackendMode", script)
        self.assertIn("Local Whisper", script)
        self.assertNotIn("Local NVIDIA Parakeet", script)
        self.assertIn("Cloud STT", script)
        self.assertIn("backend == SttBackendMode::Whisper", script)
        self.assertNotIn("SttBackendMode::Parakeet", script)
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

    def test_rust_ui_keyring_initializes_v4_platform_store(self):
        cargo = Path("src/rust/Cargo.toml").read_text(encoding="utf-8")

        api_keys = Path("src/rust/ui/api_keys.rs").read_text(encoding="utf-8")

        self.assertIn('keyring-core = "1.0"', cargo)
        self.assertIn('windows-native-keyring-store = "1.1"', cargo)
        self.assertIn('apple-native-keyring-store = { version = "1.0", features = ["keychain"] }', cargo)
        self.assertIn('zbus-secret-service-keyring-store = { version = "1.0", features = ["crypto-rust"] }', cargo)
        self.assertIn("use keyring_core::{set_default_store, Entry, Error};", api_keys)
        self.assertIn("Err(poisoned) => poisoned.into_inner()", api_keys)
        self.assertIn("windows_native_keyring_store::Store::new()", api_keys)
        self.assertIn("apple_native_keyring_store::keychain::Store::new()", api_keys)
        self.assertIn("zbus_secret_service_keyring_store::Store::new()", api_keys)
        self.assertNotIn('keyring = "4.0"', cargo)
        self.assertNotIn('"windows-native"', cargo)
        self.assertNotIn('"apple-native"', cargo)
        self.assertNotIn('"linux-native-sync-persistent"', cargo)

    def test_rust_ui_has_cloud_api_test_and_local_viewers(self):
        ui = rust_ui_source()
        lib = Path("src/rust/lib.rs").read_text(encoding="utf-8")
        cargo = Path("src/rust/Cargo.toml").read_text(encoding="utf-8")

        self.assertIn("pub mod cloud_api;", lib)
        self.assertIn("pub mod telemetry;", lib)
        self.assertIn('ureq = { version = "3.3"', cargo)
        self.assertIn('"Test cloud API"', ui)
        self.assertIn("fn run_cloud_api_check(&mut self)", ui)
        self.assertIn("check_cloud_api(&check)", ui)
        self.assertIn('"Test post API"', ui)
        self.assertIn("check_post_api(&check)", ui)
        self.assertIn('"Preview history"', ui)
        self.assertIn('"Preview metrics"', ui)
        self.assertIn("telemetry::preview_jsonl", ui)
        # Clicking a Preview button scrolls the freshly loaded preview (which sits
        # below the settings ScrollArea fold) into view via a one-shot flag, so the
        # click is visibly effective instead of reading as "did nothing".
        self.assertIn("scroll_to_history_preview", ui)
        self.assertIn("scroll_to_metrics_preview", ui)
        self.assertIn("response.scroll_to_me(Some(egui::Align::Center))", ui)

    def test_rust_ui_has_single_diagnostics_level_dropdown_on_system_tab(self):
        ui = rust_ui_source()

        # The two raw debug toggles (VOICEPI_DEBUG / VOICEPI_STT_DEBUG) were
        # consolidated into ONE ordered "Diagnostics" level dropdown. The combo is
        # a pure UI affordance over the still-persisted `debug` / `stt_debug`
        # bools, so those env-named checkbox rows must be gone everywhere.
        self.assertNotIn('"VOICEPI_DEBUG"', ui)
        self.assertNotIn('"VOICEPI_STT_DEBUG"', ui)
        self.assertIn("fn diagnostics_combo(", ui)
        self.assertIn("enum DiagnosticsLevel", ui)
        # The level→bools mapping now spans THREE persisted bools (debug,
        # stt_debug, trace) so the dropdown can offer a 4th "Trace" level; match
        # each parameter independently (rustfmt may wrap the signature).
        self.assertIn("fn diagnostics_level(", ui)
        self.assertIn("debug: bool,", ui)
        self.assertIn("stt_debug: bool,", ui)
        self.assertIn("trace: bool,", ui)
        self.assertIn("fn apply_diagnostics_level(", ui)
        self.assertIn("DiagnosticsLevel::Trace", ui)
        self.assertIn("UiTextKey::Diagnostics", ui)
        self.assertIn('from_id_salt("diagnostics_level")', ui)
        # The underlying persisted fields are untouched (config + worker + env vars
        # keep working), so all three bools must still be written by the combo.
        self.assertIn("self.settings.debug = debug;", ui)
        self.assertIn("self.settings.stt_debug = stt_debug;", ui)
        self.assertIn("self.settings.trace = trace;", ui)

        # Diagnostics is an APP-LEVEL concern, so the combo now lives on the
        # System tab (next to Integration), NOT on the Output tab which is pure
        # speech-output. Assert the placement by file.
        system = Path("src/rust/ui/tabs/system.rs").read_text(encoding="utf-8")
        output = Path("src/rust/ui/tabs/output.rs").read_text(encoding="utf-8")
        self.assertIn("fn diagnostics_combo(", system)
        self.assertIn('settings_grid("system_diagnostics_settings")', system)
        self.assertIn("self.diagnostics_combo(ui)", system)
        self.assertNotIn("diagnostics_combo", output)
        self.assertNotIn("diagnostics_level", output)

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
            # "Parakeet model" was removed in Wave 8 of #348 together with the backend.
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
        # UI language is now an extensible dropdown (ComboBox), replacing the old
        # two-button language_toggle.
        self.assertNotIn("fn language_toggle(", script)
        self.assertIn('egui::ComboBox::from_id_salt("ui_language_select")', script)
        self.assertIn('"UI language"', script)
        self.assertIn('("", "Auto")', script)
        self.assertIn('("da", "Danish")', script)
        self.assertIn('if !cfg!(windows) {', script)
        self.assertIn('"Linux keyboard layout"', script)
        # Old flat section labels replaced by scope_group boxes.
        self.assertNotIn('"Local runtime"', script)
        self.assertNotIn('"Dictation controls"', script)
        self.assertNotIn("Applies to local and cloud speech engines.", script)
        # New Speech tab scope-group structure (headings via UiTextKey, unique grid IDs).
        # Wave 8 of #348 removed the SpeechGroupParakeet heading + "speech_parakeet"
        # grid id together with the backend.
        self.assertIn("UiTextKey::SpeechGroupWhisper", script)
        self.assertNotIn("UiTextKey::SpeechGroupParakeet", script)
        self.assertIn("UiTextKey::SpeechGroupOnline", script)
        self.assertIn("UiTextKey::SpeechGroupGeneral", script)
        self.assertIn('"speech_whisper"', script)
        self.assertNotIn('"speech_parakeet"', script)
        self.assertIn('"speech_online"', script)
        self.assertIn('"speech_general"', script)
        # Device + Compute type are in the General group, used by the local
        # Whisper backend (Parakeet was dropped in Wave 8 of #348).
        self.assertIn("backend != SttBackendMode::Cloud", script)
        # Microphone + Refresh devices remain in the General group.
        self.assertIn('"Refresh devices"', script)
        self.assertIn('"Wayland ydotool/XKB layout used for direct text injection on Linux.', script)

    def test_rust_speech_tab_scope_groups_field_placement(self):
        """Encode the per-group field assignment so a future regrouping is caught."""
        script = rust_ui_source()

        # Whisper group contains the Whisper model picker. Wave 8 of #348 dropped
        # the Parakeet group, so the upper bound here is `speech_online`.
        whisper_group = script.split('"speech_whisper"', 1)[1].split('"speech_online"', 1)[0]
        self.assertIn('"Whisper model"', whisper_group)
        self.assertIn("SttBackendMode::Whisper", whisper_group)

        # Online group contains cloud provider, model, URL, timeout, API key.
        # The key action buttons live in cloud_stt_key_section (a helper called
        # from inside the online group closure), so check them in full script.
        online_group = script.split('"speech_online"', 1)[1].split('"speech_general"', 1)[0]
        self.assertIn('"Cloud STT provider"', online_group)
        self.assertIn('"Cloud STT model"', online_group)
        self.assertIn('"Cloud STT API URL"', online_group)
        self.assertIn('"Cloud STT timeout ms"', online_group)
        self.assertIn('"Cloud STT API key"', online_group)
        # cloud_stt_key_section is called from the online group closure.
        self.assertIn("self.cloud_stt_key_section(ui, provider)", online_group)
        # The buttons are defined in the helper method itself (present in script).
        self.assertIn('"Save API key"', script)
        self.assertIn('"Test cloud API"', script)
        self.assertIn("SttBackendMode::Cloud", online_group)

        # General group contains Device, Compute type, Microphone, Language,
        # Hotkey, Toggle mode, Quit key, Quit count, Quit window ms.
        general_group = script.split('"speech_general"', 1)[1]
        self.assertIn('"Device"', general_group)
        self.assertIn('"Compute type"', general_group)
        self.assertIn('"Microphone"', general_group)
        self.assertIn('"Refresh devices"', general_group)
        self.assertIn('"Language"', general_group)
        self.assertIn('"Hotkey"', general_group)
        self.assertIn('"Toggle mode"', general_group)
        self.assertIn('"Quit key"', general_group)
        self.assertIn('"Quit count"', general_group)
        self.assertIn('"Quit window ms"', general_group)
        # Device and Compute type are greyed when cloud backend is active.
        self.assertIn("backend != SttBackendMode::Cloud", general_group)

    def test_rust_quality_tab_has_scope_groups_and_text_scale_stepper(self):
        """Quality tab must use three scope_group headings with the exact grid IDs
        from quality.rs; a future regrouping regression should fail this test."""
        script = rust_ui_source()

        # Wave 8 of #348 collapsed the Quality tab to two scope_group headings
        # (AllBackends + Whisper); the Parakeet group is gone.
        self.assertIn("UiTextKey::QualityGroupAllBackends", script)
        self.assertIn("UiTextKey::QualityGroupWhisper", script)
        self.assertNotIn("UiTextKey::QualityGroupParakeet", script)
        # Exact grid id_salt values used by scope_group in quality.rs.
        self.assertIn('"quality_all_backends"', script)
        self.assertIn('"quality_whisper"', script)
        self.assertNotIn('"quality_parakeet"', script)
        # text_scale_stepper must exist in the UI source (lives in system.rs after
        # the text_scale.rs refactor so it falls under the coverage exclusion).
        self.assertIn("fn text_scale_stepper(", script)

    def test_rust_short_value_combos_use_narrow_width(self):
        """Short-enum dropdowns (auto/type/paste, Off/Basic/Verbose, auto/cuda/cpu)
        use a fixed NARROW width instead of stretching the full control width, while
        long descriptive option labels (model pickers, compute type) stay WIDE."""
        script = rust_ui_source()

        # The narrow-width machinery exists.
        self.assertIn("const SETTINGS_SHORT_CONTROL_WIDTH: f32 = 240.0;", script)
        self.assertIn("fn settings_short_control_width(", script)
        self.assertIn("enum ComboWidth", script)

        # Short-enum combos opt into the narrow variants.
        self.assertIn("fn combo_help_short(", script)
        self.assertIn("fn combo_enabled_short(", script)
        self.assertIn("fn combo_help_labeled_short(", script)
        self.assertIn("fn combo_enabled_labeled_short(", script)

        # Output short combos.
        output = Path("src/rust/ui/tabs/output.rs").read_text(encoding="utf-8")
        self.assertIn('combo_help_short(\n                    ui,\n                    "Inject mode"', output)
        self.assertIn('combo_help_short(\n                    ui,\n                    "Format commands"', output)

        # Speech short combos (Device, Cloud STT provider, Language, layout).
        speech = Path("src/rust/ui/tabs/speech.rs").read_text(encoding="utf-8")
        self.assertIn("combo_enabled_short(", speech)
        self.assertIn("combo_enabled_labeled_short(", speech)
        self.assertIn("combo_help_labeled_short(", speech)
        self.assertIn('"Device"', speech)
        # WIDE combos with long option labels stay wide (no _short suffix).
        self.assertIn('combo_enabled_labeled(\n                    ui,\n                    backend != SttBackendMode::Cloud,\n                    "Compute type"', speech)
        self.assertIn("combo_model_vram(", speech)

        # Post mode is short; Post model stays wide.
        post = Path("src/rust/ui/tabs/post.rs").read_text(encoding="utf-8")
        self.assertIn('combo_enabled_short(\n                    ui,\n                    post_enabled,\n                    "Post mode"', post)

        # The label-column alignment anchor must still be in place so value
        # columns line up across groups and tabs after the width change.
        self.assertIn("ui.set_min_width(settings_label_width(ui))", script)
