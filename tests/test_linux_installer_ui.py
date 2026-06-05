from tests.test_helpers import (
    os,
    Path,
    subprocess,
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

class RustUiInstallerTests(unittest.TestCase):
    def test_linux_rust_ui_installer_builds_release_binary_and_desktop_entry(self):
        path = Path("scripts/linux/install-rust-ui.sh")
        script = path.read_text(encoding="utf-8")

        self.assertTrue(os.access(path, os.X_OK))
        mode = subprocess.check_output(
            ["git", "ls-files", "--stage", path.as_posix()],
            text=True,
        ).split()[0]
        self.assertEqual("100755", mode)
        self.assertIn('SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"', script)
        self.assertIn('HERE="$(cd "${SCRIPT_DIR}/../.." && pwd)"', script)
        self.assertIn('HERE="$(cd "${SCRIPT_DIR}/.." && pwd)"', script)
        self.assertIn('if [[ -x "${HERE}/whisper-dictate" ]]; then', script)
        self.assertIn("cargo build --release -p whisper-dictate-app", script)
        self.assertIn('REAL_BIN="${LIB_DIR}/whisper-dictate-app"', script)
        self.assertIn('SOURCE_BIN="${HERE}/target/release/whisper-dictate"', script)
        self.assertIn('install -m 0755 "${SOURCE_BIN}" "${REAL_BIN}"', script)
        self.assertIn('install -m 0644 "${HERE}/assets/whisper-dictate-logo.svg" "${ICON}"', script)
        self.assertIn('export VOICEPI_APP_ROOT="${HERE}"', script)
        self.assertIn('exec "${REAL_BIN}" "\\$@"', script)
        self.assertIn("whisper-dictate.desktop", script)
        self.assertIn("Exec=${BIN} ui", script)
        self.assertIn("Icon=${ICON}", script)
        self.assertIn("StartupWMClass=whisper-dictate", script)
        self.assertIn("ensure_user_bin_first", script)
        self.assertIn('${HOME}/.zprofile', script)
        self.assertIn('export PATH="${HOME}/.local/bin:${PATH}"', script)
        self.assertIn('Run now: ${BIN} ui', script)
        self.assertNotIn("setup.ps1", script)

    def test_rust_ui_sets_linux_app_id_for_desktop_shells(self):
        ui = rust_ui_source()

        self.assertIn('.with_app_id("whisper-dictate")', ui)

    def test_ubuntu_setup_resets_stale_ydotoold_before_starting_service(self):
        script = Path("ubuntu26.04/setup.sh").read_text(encoding="utf-8")

        self.assertIn("systemctl --user stop ydotoold.service", script)
        self.assertIn("systemctl --user reset-failed ydotoold.service", script)
        self.assertIn("pkill -KILL -x ydotoold", script)
        self.assertIn('rm -f "$YDOTOOL_SOCKET_PATH"', script)
        self.assertIn("systemctl --user restart ydotoold.service", script)

    def test_linux_ui_docs_point_to_rust_ui_not_pyside_powershell(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")

        self.assertIn("scripts/linux/install-rust-ui.sh", readme)
        self.assertIn("whisper-dictate ui", readme)
        self.assertIn("scripts/linux/install-rust-ui.sh", config)
        self.assertNotIn("On Linux/macOS, install\n`requirements-ui.txt`", readme)
        self.assertNotIn("setup.ps1 --settings-ui", readme)

    def test_technical_docs_include_rust_platform_capability_matrix(self):
        technical = Path("docs/TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("Rust desktop platform capability matrix", technical)
        self.assertIn("| Capability | Windows 10/11 | Linux Wayland | Linux X11 |", technical)
        self.assertIn("whisper-dictate run -- ...", technical)
        self.assertIn("scripts/linux/install-rust-ui.sh", technical)
        self.assertIn("scripts/windows/build-installer.ps1", technical)

    def test_groq_provider_is_persisted_and_key_is_not_plain_config(self):
        ui = rust_ui_source()
        api_keys = Path("src/rust/whisper-dictate-app/src/ui/api_keys.rs").read_text(encoding="utf-8")
        config = Path("src/rust/whisper-dictate-app/src/config.rs").read_text(encoding="utf-8")

        self.assertIn('"Cloud STT provider"', ui)
        self.assertIn("fn set_cloud_provider(&mut self, provider: CloudProvider)", ui)
        self.assertIn('self.settings.stt_provider = provider.id().to_owned();', ui)
        self.assertIn('self.settings.stt_backend = "openai".to_owned();', ui)
        self.assertIn("self.settings.stt_model = provider.default_model().to_owned();", ui)
        self.assertIn("set_string(object, \"stt_provider\", &self.stt_provider);", config)
        self.assertIn("keyring::Entry::new", api_keys)
        self.assertNotIn("stt_api_key", config)
