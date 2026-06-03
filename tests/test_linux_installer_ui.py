from tests.test_helpers import *

def rust_ui_source():
    paths = [
        "crates/whisper-dictate-app/src/ui.rs",
        "crates/whisper-dictate-app/src/ui/tabs.rs",
        "crates/whisper-dictate-app/src/ui/api_keys.rs",
        "crates/whisper-dictate-app/src/ui/icon.rs",
    ]
    return "\n".join(Path(path).read_text(encoding="utf-8") for path in paths)

class RustUiInstallerTests(unittest.TestCase):
    def test_linux_rust_ui_installer_builds_release_binary_and_desktop_entry(self):
        path = Path("scripts/install-linux-rust-ui.sh")
        script = path.read_text(encoding="utf-8")

        self.assertTrue(os.access(path, os.X_OK))
        self.assertIn("cargo build --release -p whisper-dictate-app", script)
        self.assertIn('REAL_BIN="${LIB_DIR}/whisper-dictate-app"', script)
        self.assertIn('install -m 0755 "${HERE}/target/release/whisper-dictate" "${REAL_BIN}"', script)
        self.assertIn('export VOICEPI_APP_ROOT="${HERE}"', script)
        self.assertIn('exec "${REAL_BIN}" "\\$@"', script)
        self.assertIn("whisper-dictate.desktop", script)
        self.assertIn("Exec=${BIN} ui", script)
        self.assertNotIn("setup.ps1", script)

    def test_linux_ui_docs_point_to_rust_ui_not_pyside_powershell(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("CONFIGURATION.md").read_text(encoding="utf-8")

        self.assertIn("scripts/install-linux-rust-ui.sh", readme)
        self.assertIn("whisper-dictate ui", readme)
        self.assertIn("scripts/install-linux-rust-ui.sh", config)
        self.assertNotIn("On Linux/macOS, install\n`requirements-ui.txt`", readme)
        self.assertNotIn("setup.ps1 --settings-ui", readme)

    def test_technical_docs_include_rust_platform_capability_matrix(self):
        technical = Path("TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("Rust desktop platform capability matrix", technical)
        self.assertIn("| Capability | Windows 10/11 | Linux Wayland | Linux X11 |", technical)
        self.assertIn("whisper-dictate run -- ...", technical)
        self.assertIn("scripts/install-linux-rust-ui.sh", technical)
        self.assertIn("scripts/build-windows-installer.ps1", technical)

    def test_groq_provider_is_persisted_and_key_is_not_plain_config(self):
        ui = rust_ui_source()
        api_keys = Path("crates/whisper-dictate-app/src/ui/api_keys.rs").read_text(encoding="utf-8")
        config = Path("crates/whisper-dictate-app/src/config.rs").read_text(encoding="utf-8")

        self.assertIn('"Cloud STT provider"', ui)
        self.assertIn("fn set_cloud_provider(&mut self, provider: CloudProvider)", ui)
        self.assertIn('self.settings.stt_provider = provider.id().to_owned();', ui)
        self.assertIn('self.settings.stt_backend = "openai".to_owned();', ui)
        self.assertIn("self.settings.stt_model = provider.default_model().to_owned();", ui)
        self.assertIn("set_string(object, \"stt_provider\", &self.stt_provider);", config)
        self.assertIn("keyring::Entry::new", api_keys)
        self.assertNotIn("stt_api_key", config)
