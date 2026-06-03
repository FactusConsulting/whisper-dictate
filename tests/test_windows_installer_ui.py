from tests.test_helpers import *

def rust_ui_source():
    paths = [
        "crates/whisper-dictate-app/src/ui.rs",
        "crates/whisper-dictate-app/src/ui/tabs.rs",
        "crates/whisper-dictate-app/src/ui/api_keys.rs",
        "crates/whisper-dictate-app/src/ui/icon.rs",
    ]
    return "\n".join(Path(path).read_text(encoding="utf-8") for path in paths)

class WindowsLauncherRegressionTests(unittest.TestCase):
    def test_installer_no_longer_packages_legacy_launchers(self):
        with open("installer/whisper-dictate.iss", encoding="utf-8") as f:
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
        self.assertIn(r'Source: "..\target\release\whisper-dictate.exe"', script)
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"', script)

    def test_rust_windows_ui_uses_gui_subsystem(self):
        script = Path("crates/whisper-dictate-app/src/main.rs").read_text(encoding="utf-8")

        self.assertIn(
            '#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]',
            script,
        )

    def test_rust_background_processes_hide_windows_console(self):
        script = Path("crates/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

        self.assertIn("const CREATE_NO_WINDOW: u32 = 0x08000000;", script)
        self.assertIn("fn configure_background_process(", script)
        self.assertIn(".creation_flags(CREATE_NO_WINDOW);", script)
        self.assertIn("configure_background_process(&mut process);", script)

    def test_rust_ui_cleans_stale_desktop_processes_before_starting_window(self):
        ui_script = rust_ui_source()
        runtime_script = Path("crates/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

        self.assertLess(
            ui_script.index("runtime::cleanup_stale_desktop_processes();"),
            ui_script.index("eframe::run_native("),
        )
        self.assertIn("#[cfg(windows)]\npub fn cleanup_stale_desktop_processes()", runtime_script)
        self.assertIn("#[cfg(not(windows))]\npub fn cleanup_stale_desktop_processes() {}", runtime_script)
        self.assertIn("fn cleanup_stale_desktop_processes_windows() -> Result<()>", runtime_script)
        self.assertIn("fn stale_process_cleanup_script(", runtime_script)
        self.assertIn("$cleanupPid = $PID", runtime_script)
        self.assertIn("$_.ProcessId -ne $cleanupPid", runtime_script)
        self.assertIn("fn windows_shell_program() -> &'static str", runtime_script)
        self.assertIn('"pwsh.exe"', runtime_script)
        self.assertIn("$_.ExecutablePath -eq $exe", runtime_script)
        self.assertIn('$_.CommandLine -like "*voice_pi.py*"', runtime_script)
        self.assertIn('$_.CommandLine -like "*$root*"', runtime_script)
        runtime_without_tests = runtime_script.split("#[cfg(test)]", 1)[0]
        self.assertNotIn("Stop-Process -Name python", runtime_without_tests)

    def test_rust_runtime_log_expands_to_available_width(self):
        script = rust_ui_source()
        runtime_tab = script.split("fn runtime_tab", 1)[1].split("fn settings_panel", 1)[0]

        self.assertIn("egui::ScrollArea::both()", runtime_tab)
        self.assertIn('.id_salt("runtime_log_scroll")', runtime_tab)
        self.assertIn(".max_height(height)", runtime_tab)
        self.assertIn(".stick_to_bottom(true)", runtime_tab)
        self.assertIn(".desired_width(ui.available_width())", runtime_tab)
        self.assertIn('.id_salt("runtime_log_view")', runtime_tab)

    def test_rust_runtime_log_can_be_copied(self):
        script = rust_ui_source()

        self.assertIn('ui.button("Copy").clicked()', script)
        self.assertIn("ui.ctx().copy_text(self.runtime_log.clone())", script)
        self.assertIn("let mut runtime_log_view = self.runtime_log.clone();", script)
        self.assertIn('.id_salt("runtime_log_view")', script)
        runtime_tab = script.split("fn runtime_tab", 1)[1].split("fn settings_panel", 1)[0]
        self.assertNotIn(".interactive(false)", runtime_tab)

    def test_rust_runtime_tab_can_clear_log_without_stopping_runtime(self):
        script = rust_ui_source()

        self.assertIn('ui.button("Clear").clicked()', script)
        self.assertIn("self.runtime_log.clear();", script)

    def test_rust_ui_shows_version_in_title_and_top_bar(self):
        script = rust_ui_source()
        icon = Path("crates/whisper-dictate-app/src/ui/icon.rs").read_text(encoding="utf-8")

        self.assertIn('&format!("whisper-dictate {}", runtime::version())', script)
        self.assertIn('ui.label(format!("whisper-dictate {}", runtime::version()))', script)
        self.assertIn(".with_icon(app_icon())", script)
        self.assertIn("fn app_icon() -> egui::IconData", icon)

    def test_rust_ui_has_cloud_provider_dropdown_and_key_storage(self):
        script = rust_ui_source()
        api_keys = Path("crates/whisper-dictate-app/src/ui/api_keys.rs").read_text(encoding="utf-8")

        self.assertIn('GROQ_STT_BASE_URL: &str = "https://api.groq.com/openai/v1"', api_keys)
        self.assertIn('GROQ_STT_MODEL: &str = "whisper-large-v3-turbo"', api_keys)
        self.assertIn('OPENAI_STT_BASE_URL: &str = "https://api.openai.com/v1"', api_keys)
        self.assertIn("enum CloudProvider", api_keys)
        self.assertIn("const GROQ_STT_MODELS: &[&str]", script)
        self.assertIn("const OPENAI_STT_MODELS: &[&str]", script)
        self.assertIn("const WHISPER_MODELS: &[&str]", script)
        self.assertIn('"distil-whisper-large-v3-en"', script)
        self.assertIn('"gpt-4o-mini-transcribe"', script)
        self.assertIn('"Cloud STT provider",', script)
        self.assertIn('"Cloud STT model",', script)
        self.assertIn("provider.model_options()", script)
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
        self.assertIn("fn save_stt_api_key_now(&mut self)", script)
        self.assertIn("fn ensure_stt_api_key_loaded_for_runtime(&mut self)", script)
        self.assertIn("fn cloud_stt_missing_api_key(&self) -> bool", script)
        self.assertIn("fn save_stt_api_key_if_changed(", script)
        self.assertIn("keyring::Entry::new", api_keys)
        self.assertIn("STT_API_KEY_ENV", api_keys)
        self.assertIn("fn has_unsaved_settings(&self) -> bool", script)
        self.assertIn('egui::RichText::new("Save settings *").strong()', script)
        self.assertIn(".add_enabled(is_dirty, save_button)", script)
        self.assertIn('"Unsaved changes"', script)
        self.assertIn('ui.button("Reload from disk").clicked()', script)
        self.assertNotIn('ui.button("Clear API key").clicked()', script)

    def test_rust_ui_uses_same_api_key_loader_on_start_and_reload(self):
        script = rust_ui_source()
        api_keys = Path("crates/whisper-dictate-app/src/ui/api_keys.rs").read_text(encoding="utf-8")
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
        ui_tests = Path("crates/whisper-dictate-app/src/ui/tests.rs").read_text(encoding="utf-8")
        self.assertIn("environment_api_keys_do_not_make_settings_dirty_at_startup", ui_tests)
        self.assertIn("edited_api_key_still_makes_settings_dirty", ui_tests)

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
        api_keys = Path("crates/whisper-dictate-app/src/ui/api_keys.rs").read_text(encoding="utf-8")

        self.assertIn('const POST_API_KEY_ENV: &str = "VOICEPI_POST_API_KEY"', api_keys)
        self.assertIn("enum PostProvider", api_keys)
        self.assertIn('"Post API key"', script)
        self.assertIn('"Save post API key"', script)
        self.assertIn("fn save_post_api_key_now(&mut self)", script)
        self.assertIn("fn load_post_api_key_state(", api_keys)
        self.assertIn("fn load_post_api_key_from_env(provider: PostProvider)", api_keys)
        self.assertIn("fn save_post_api_key(provider: PostProvider", api_keys)
        self.assertIn("self.reload_post_api_key();", script)
        self.assertIn("const GROQ_POST_MODELS: &[&str]", script)
        self.assertIn('"llama-3.1-8b-instant"', script)
        self.assertIn('"llama-3.3-70b-versatile"', script)
        self.assertIn('"qwen/qwen3-32b"', script)
        self.assertIn('"openai/gpt-oss-20b"', script)
        self.assertIn('"groq/compound-mini"', script)
        self.assertIn('&["none", "ollama", "openai", "groq"]', script)
        self.assertIn('matches!(self.settings.post_processor.as_str(), "openai" | "groq")', script)
        self.assertIn("let post_key = self.post_api_key_input.trim();", script)
        self.assertIn("self.stt_api_key_input.trim()", script)
        self.assertIn("fn normalize_postprocessor_settings(&mut self)", script)
        self.assertIn("GROQ_STT_BASE_URL.to_owned()", script)
        self.assertIn("Optional second text pass after speech recognition", script)
        self.assertIn("Controls what the post processor is allowed to do", script)
        self.assertIn("response.on_hover_text(help)", script)
        self.assertIn('"Quit key"', script)

    def test_rust_ui_has_cloud_api_test_and_local_viewers(self):
        ui = rust_ui_source()
        lib = Path("crates/whisper-dictate-app/src/lib.rs").read_text(encoding="utf-8")
        cargo = Path("crates/whisper-dictate-app/Cargo.toml").read_text(encoding="utf-8")

        self.assertIn("pub mod cloud_api;", lib)
        self.assertIn("pub mod telemetry;", lib)
        self.assertIn('ureq = { version = "2.12"', cargo)
        self.assertIn('"Test cloud API"', ui)
        self.assertIn("fn run_cloud_api_check(&mut self)", ui)
        self.assertIn("check_cloud_api(&check)", ui)
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
        self.assertIn("fn apply_ui_text_scale(", script)
        self.assertIn("DEFAULT_UI_TEXT_SCALE", script)
        self.assertIn("style.text_styles = text_styles", script)
        self.assertNotIn(".small().color(ui.visuals().weak_text_color())", script)
        self.assertIn("data.insert_persisted(id, show_help)", script)
        self.assertIn("response.on_hover_text(help)", script)
        for label in (
            "STT backend",
            "Whisper model",
            "Parakeet model",
            "Cloud STT model",
            "Beam size",
            "Audio ducking",
            "Audio ducking level",
            "Initial prompt",
            "Dictionary path",
            "Dictionary enabled",
            "Inject mode",
            "JSON stdout",
            "Cloud redaction",
            "Redaction terms",
            "UI text scale",
            "Profiles JSON",
        ):
            self.assertIn(label, script)

    def test_config_maps_audio_ducking_and_cloud_redaction(self):
        config = Path("vp_config.py").read_text(encoding="utf-8")
        rust_config = Path("crates/whisper-dictate-app/src/config.rs").read_text(encoding="utf-8")
        ui = rust_ui_source()

        for token in (
            "VOICEPI_AUDIO_DUCKING",
            "VOICEPI_AUDIO_DUCKING_LEVEL",
            "VOICEPI_POST_REDACT",
            "VOICEPI_POST_REDACT_TERMS",
        ):
            self.assertIn(token, config)
        for key in (
            "audio_ducking",
            "audio_ducking_level",
            "post_redact",
            "post_redact_terms",
        ):
            self.assertIn(key, rust_config)
            self.assertIn(key, ui)

    def test_rust_cli_has_explicit_ubuntu_setup_command(self):
        cli = Path("crates/whisper-dictate-app/src/cli.rs").read_text(encoding="utf-8")
        main = Path("crates/whisper-dictate-app/src/main.rs").read_text(encoding="utf-8")
        runtime = Path("crates/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

        self.assertIn("SetupUbuntu", cli)
        self.assertIn('["whisper-dictate", "setup-ubuntu"]', cli)
        self.assertIn("Command::SetupUbuntu => runtime::setup_ubuntu()", main)
        self.assertIn("pub fn setup_ubuntu() -> Result<()>", runtime)
        self.assertIn('join("ubuntu26.04").join("setup.sh")', runtime)
        self.assertIn('env("VOICEPI_RUST_OWNS_DESKTOP", "1")', runtime)
        self.assertIn("fn install_linux_desktop_entries() -> Result<()>", runtime)
        self.assertIn("fn linux_desktop_entry(autostart: bool) -> String", runtime)
        self.assertIn("fn start_linux_ui_detached() -> Result<()>", runtime)

    def test_ubuntu_setup_creates_launcher_autostart_and_starts_rust_ui(self):
        script = Path("ubuntu26.04/setup.sh").read_text(encoding="utf-8")
        runtime = Path("crates/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

        self.assertIn('VOICEPI_RUST_OWNS_DESKTOP', script)
        self.assertIn('Exec=whisper-dictate ui', runtime)
        self.assertIn('Name=Whisper Dictate', runtime)
        self.assertIn('.local/share/applications', runtime)
        self.assertIn('.config/autostart', runtime)
        self.assertIn('gtk-launch', runtime)
        self.assertIn('setsid', runtime)
        self.assertIn('Terminal-runtime: whisper-dictate run -- --key shift_r+ctrl_r --lang da', script)
        self.assertNotIn('Exec=whisper-dictate --key shift_r+ctrl_r --lang da', script)

    def test_windows_docs_use_rust_terminal_entrypoint(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("CONFIGURATION.md").read_text(encoding="utf-8")
        technical = Path("TECHNICAL.md").read_text(encoding="utf-8")

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
        config = Path("CONFIGURATION.md").read_text(encoding="utf-8")

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
        config = Path("CONFIGURATION.md").read_text(encoding="utf-8")

        for doc in (readme, config):
            self.assertIn("whisper-dictate setup-ubuntu", doc)
            self.assertIn("Whisper Dictate", doc)
            self.assertIn("whisper-dictate ui", doc)
        self.assertIn("Then press **Start** in the Runtime tab", readme)

    def test_installer_uses_whisper_dictate_icon_and_searchable_ui_name(self):
        with open("installer/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r"SetupIconFile=..\assets\whisper-dictate.ico", script)
        self.assertIn(r'Source: "..\assets\whisper-dictate.ico"', script)
        self.assertIn(r'IconFilename: "{app}\whisper-dictate.ico"', script)
        self.assertNotIn(r"Legacy Settings UI", script)
        self.assertNotIn(r"\Settings UI", script)

    def test_windows_icon_is_multiresolution_and_has_source_logo(self):
        icon = Path("assets/whisper-dictate.ico").read_bytes()
        svg = Path("assets/whisper-dictate-logo.svg").read_text(encoding="utf-8")

        self.assertGreater(len(icon), 90_000)
        self.assertEqual(int.from_bytes(icon[0:2], "little"), 0)
        self.assertEqual(int.from_bytes(icon[2:4], "little"), 1)
        self.assertEqual(int.from_bytes(icon[4:6], "little"), 7)
        sizes = {
            256 if icon[6 + i * 16] == 0 else icon[6 + i * 16]
            for i in range(7)
        }
        self.assertEqual(sizes, {16, 24, 32, 48, 64, 128, 256})
        self.assertIn("viewBox=\"0 0 256 256\"", svg)
        self.assertIn("linearGradient", svg)
        self.assertIn("fill=\"#FFFFFF\"", svg)

    def test_rust_windows_binary_embeds_application_icon_resource(self):
        cargo = Path("crates/whisper-dictate-app/Cargo.toml").read_text(encoding="utf-8")
        build = Path("crates/whisper-dictate-app/build.rs").read_text(encoding="utf-8")

        self.assertIn("winresource", cargo)
        self.assertIn("CARGO_CFG_TARGET_OS", build)
        self.assertIn('"windows"', build)
        self.assertIn("../../assets/whisper-dictate.ico", build)
        self.assertIn("resource.compile()", build)

    def test_github_docs_show_logo(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        release_notes = Path("RELEASE_NOTES.md").read_text(encoding="utf-8")

        self.assertIn('src="assets/whisper-dictate-logo.svg"', readme)
        self.assertIn("<h1 align=\"center\">whisper-dictate</h1>", readme)
        self.assertIn('src="assets/whisper-dictate-logo.svg"', release_notes)

    def test_installer_creates_desktop_ui_shortcut(self):
        with open("installer/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r'Name: "{userdesktop}\whisper-dictate"', script)
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"', script)
        self.assertIn(r'Parameters: "ui"', script)

    def test_installer_packages_rust_ui_as_primary_desktop_entry(self):
        with open("installer/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r'Source: "..\target\release\whisper-dictate.exe"', script)
        self.assertIn(
            r'Name: "{userprograms}\whisper-dictate\whisper-dictate";    Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"',
            script,
        )
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"; Description: "Launch whisper-dictate now"', script)

    def test_windows_installer_workflows_build_rust_ui_before_inno(self):
        for path in (".github/workflows/release.yml", ".github/workflows/windows-installer.yml"):
            workflow = Path(path).read_text(encoding="utf-8")
            rust_build = workflow.index("cargo build --release -p whisper-dictate-app")
            installer_build = workflow.index("Build installers")
            self.assertLess(rust_build, installer_build)
            self.assertIn("Cargo.toml Cargo.lock crates/", workflow)

        script = Path("scripts/build-windows-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("cargo build --release -p whisper-dictate-app", script)
        self.assertIn("cargo build failed", script)

    def test_windows_zip_packages_are_built_on_windows_with_rust_exe(self):
        for path in (".github/workflows/release.yml", ".github/workflows/windows-installer.yml"):
            workflow = Path(path).read_text(encoding="utf-8")

            self.assertIn("Build Windows ZIP packages", workflow)
            self.assertIn("whisper-dictate-windows-$version.zip", workflow)
            self.assertIn("whisper-dictate-windows-setup-$version.exe", workflow)
            self.assertIn("Copy-Item target\\release\\whisper-dictate.exe", workflow)
            self.assertIn("Copy-Item assets\\whisper-dictate.ico", workflow)
            self.assertIn('Copy-Item requirements-cpu.txt (Join-Path $bundle "requirements.txt")', workflow)
            self.assertIn("Copy-Item requirements-cpu.txt,requirements-gpu.txt", workflow)
            self.assertIn("Output/*.exe Output/*.zip sha256sums.txt", workflow)

        script = Path("scripts/build-windows-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("Building unified Windows portable ZIP version $Version", script)
        self.assertIn("whisper-dictate-windows-$Version.zip", script)
        self.assertIn("whisper-dictate-windows-setup-$Version.exe", script)
        self.assertIn("target\\release\\whisper-dictate.exe", script)
        self.assertIn("assets\\whisper-dictate.ico", script)
        self.assertIn("Compress-Archive", script)

    def test_docs_describe_windows_zip_and_installer_outputs(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        release_notes = Path("RELEASE_NOTES.md").read_text(encoding="utf-8")
        agents = Path("AGENTS.md").read_text(encoding="utf-8")
        technical = Path("TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("portable Windows ZIP bundle", readme)
        self.assertIn("installer and portable ZIP are written to `Output\\`", readme)
        self.assertIn("whisper-dictate-windows-<version>.zip", release_notes)
        self.assertIn("whisper-dictate-linux-<version>.zip", release_notes)
        self.assertIn("Output\\*.exe` and `Output\\*.zip", agents)
        self.assertIn("Output\\*.exe` and `Output\\*.zip", technical)

    def test_voice_pi_reconfigures_windows_streams_to_utf8(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn('reconfigure(encoding="utf-8", errors="replace")', script)

    def test_voice_pi_has_parakeet_min_duration_and_backend_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("self.parakeet_min_seconds", script)
        self.assertIn("too short for Parakeet", script)
        self.assertIn("stt_backend=self.stt_backend", script)

    def test_voice_pi_has_live_release_tail_padding(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("self.release_tail_ms", script)
        self.assertIn('after.get("release_tail_ms", "200")', script)
        self.assertIn("time.sleep(tail_s)", script)

    def test_cli_debug_prints_parakeet_min_seconds(self):
        with open("vp_cli.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("parakeet_min_s", script)
        self.assertIn("VOICEPI_PARAKEET_MIN_SECONDS", script)
        self.assertIn("release_tail_ms", script)
        self.assertIn("VOICEPI_RELEASE_TAIL_MS", script)
