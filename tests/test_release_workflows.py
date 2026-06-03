from tests.test_helpers import (
    Path,
    unittest,
)

class RustReleaseWorkflowTests(unittest.TestCase):
    def test_release_uploads_linux_rust_ui_binary(self):
        workflow = Path(".github/workflows/release.yml").read_text(encoding="utf-8")

        self.assertIn("cargo build --release -p whisper-dictate-app", workflow)
        self.assertIn("whisper-dictate-linux-rust-ui-${VERSION}", workflow)
        self.assertIn('install -m 0755 target/release/whisper-dictate "$d/whisper-dictate"', workflow)
        self.assertIn('INCLUDE_RUST_UI=1 mkbundle "whisper-dictate-linux-${VERSION}.zip"', workflow)
        self.assertIn('[ -f requirements-cpu.txt ] && cp requirements-cpu.txt "$d/"', workflow)
        self.assertIn('[ -f requirements-gpu.txt ] && cp requirements-gpu.txt "$d/"', workflow)
        self.assertIn('cp assets/whisper-dictate-logo.svg "$d/assets/"', workflow)
        self.assertIn("scripts/install-linux-rust-ui.sh", workflow)
        self.assertIn('cp ubuntu26.04/setup.sh "$d/ubuntu26.04/"', workflow)
        self.assertIn("bash -n scripts/install-linux-rust-ui.sh", workflow)
        self.assertIn("bash -n ubuntu26.04/setup.sh", workflow)

    def test_homebrew_formula_installs_linux_release_bundle(self):
        workflow = Path(".github/workflows/release.yml").read_text(encoding="utf-8")
        bump_step = workflow.split("- name: Bump Homebrew tap", 1)[1].split("windows-installer:", 1)[0]

        self.assertIn('asset="whisper-dictate-linux-${VERSION}.zip"', bump_step)
        self.assertIn("releases/download/${TAG}/${asset}", bump_step)
        self.assertIn('url "${src}"', bump_step)
        self.assertIn('payload = Dir["whisper-dictate/*"]', bump_step)
        self.assertIn('payload = Dir["*"] if payload.empty?', bump_step)
        self.assertIn("libexec.install payload", bump_step)
        self.assertIn('export VOICEPI_BOOTSTRAP_PYTHON="#{py}"', bump_step)
        self.assertIn('export VOICEPI_APP_ROOT="#{libexec}"', bump_step)
        self.assertNotIn('export VOICEPI_PYTHON="#{py}"', bump_step)
        self.assertIn('exec "#{libexec}/whisper-dictate" "\\$@"', bump_step)
        self.assertIn("install_linux_app_icon() {", bump_step)
        self.assertIn('cp "\\$icon_src" "\\$home/.local/share/icons/hicolor/scalable/apps/whisper-dictate.svg"', bump_step)
        self.assertIn("repair_linux_desktop_entry() {", bump_step)
        self.assertIn('local exec_path="#{opt_bin}/whisper-dictate"', bump_step)
        self.assertIn('grep -Fq "Exec=\\${exec_path} ui"', bump_step)
        self.assertIn('grep -Fq "Icon=whisper-dictate"', bump_step)
        self.assertIn('grep -Fq "StartupWMClass=whisper-dictate"', bump_step)
        self.assertIn('repair_linux_desktop_entry "\\${HOME:-}/.local/share/applications/whisper-dictate.desktop" 0', bump_step)
        self.assertIn('repair_linux_desktop_entry "\\${HOME:-}/.config/autostart/whisper-dictate.desktop" 1', bump_step)
        self.assertIn("def post_install", bump_step)
        self.assertIn('homes = [ENV["HOME"], *Dir["/home/*"]]', bump_step)
        self.assertIn("install_linux_app_icon(home)", bump_step)
        self.assertIn('opt_bin/"whisper-dictate"', bump_step)
        self.assertNotIn('repair_linux_desktop_entry(\n                  Pathname.new(home)/".local/share/applications/whisper-dictate.desktop",\n                  bin/"whisper-dictate"', bump_step)
        self.assertIn("File.write(path.to_s", bump_step)
        self.assertIn("Icon=whisper-dictate", bump_step)
        self.assertIn("StartupWMClass=whisper-dictate", bump_step)
        self.assertIn('raw.include?("Icon=whisper-dictate")', bump_step)
        self.assertIn('raw.include?("StartupWMClass=whisper-dictate")', bump_step)
        self.assertIn("def install_linux_app_icon(home)", bump_step)
        self.assertNotIn("path.write <<~DESKTOP", bump_step)
        self.assertNotIn("DESKTOP\n              )", bump_step)
        self.assertIn("def autostart_enabled_line(raw)", bump_step)
        self.assertIn('assert_path_exists libexec/"ubuntu26.04/setup.sh"', bump_step)
        self.assertNotIn("archive/refs/tags", bump_step)
        self.assertNotIn('exec "#{libexec}/setup.sh"', bump_step)

    def test_crate_lockfile_stays_in_sync_with_workspace_lockfile(self):
        root_lock = Path("Cargo.lock").read_text(encoding="utf-8")
        crate_lock = Path("crates/whisper-dictate-app/Cargo.lock").read_text(
            encoding="utf-8"
        )

        self.assertEqual(crate_lock, root_lock)

    def test_workflows_use_node24_checkout_action(self):
        for path in Path(".github/workflows").glob("*.yml"):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("actions/checkout@v4", workflow, path.as_posix())
            self.assertIn("actions/checkout@v5", workflow, path.as_posix())

    def test_workflows_use_node24_python_action(self):
        for path in Path(".github/workflows").glob("*.yml"):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("actions/setup-python@v5", workflow, path.as_posix())
        workflow_text = "\n".join(
            path.read_text(encoding="utf-8")
            for path in Path(".github/workflows").glob("*.yml")
        )
        self.assertIn("actions/setup-python@v6", workflow_text)

    def test_write_permissions_are_job_scoped(self):
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            pre_jobs = workflow.split("\njobs:", 1)[0]
            self.assertNotIn("contents: write", pre_jobs, path.as_posix())
            self.assertIn("permissions:\n      contents: write", workflow, path.as_posix())

    def test_windows_workflows_pin_current_windows_runner(self):
        for path in Path(".github/workflows").glob("*.yml"):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("windows-latest", workflow, path.as_posix())
            self.assertNotIn("windows-2025-vs2026", workflow, path.as_posix())
        workflow_text = "\n".join(
            path.read_text(encoding="utf-8")
            for path in Path(".github/workflows").glob("*.yml")
        )
        self.assertIn("windows-2025", workflow_text)
