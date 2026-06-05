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
        self.assertIn('cp -r requirements "$d/"', workflow)
        self.assertIn('cp assets/whisper-dictate-logo.svg "$d/assets/"', workflow)
        self.assertIn("scripts/linux/install-rust-ui.sh", workflow)
        self.assertIn('cp ubuntu26.04/setup.sh "$d/ubuntu26.04/"', workflow)
        self.assertIn("bash -n scripts/linux/install-rust-ui.sh", workflow)
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
        self.assertIn('local icon_path="\\$home/.local/share/icons/hicolor/scalable/apps/whisper-dictate.svg"', bump_step)
        self.assertIn('cp "\\$icon_src" "\\$icon_path"', bump_step)
        self.assertIn("repair_linux_desktop_entry() {", bump_step)
        self.assertIn('local exec_path="#{opt_bin}/whisper-dictate"', bump_step)
        self.assertIn('local icon_path="\\${HOME:-}/.local/share/icons/hicolor/scalable/apps/whisper-dictate.svg"', bump_step)
        self.assertIn('grep -Fq "Exec=\\${exec_path} ui"', bump_step)
        self.assertIn('grep -Fq "Icon=\\${icon_path}"', bump_step)
        self.assertIn('grep -Fq "StartupWMClass=whisper-dictate"', bump_step)
        self.assertIn('repair_linux_desktop_entry "\\${HOME:-}/.local/share/applications/whisper-dictate.desktop" 0', bump_step)
        self.assertIn('repair_linux_desktop_entry "\\${HOME:-}/.config/autostart/whisper-dictate.desktop" 1', bump_step)
        self.assertIn("def post_install", bump_step)
        self.assertIn('homes = [ENV["HOME"], *Dir["/home/*"]]', bump_step)
        self.assertIn("install_linux_app_icon(home)", bump_step)
        self.assertIn("def repair_linux_desktop_entry(path, exe, autostart, home)", bump_step)
        self.assertIn("linux_app_icon_path(home)", bump_step)
        self.assertIn('opt_bin/"whisper-dictate"', bump_step)
        self.assertNotIn('repair_linux_desktop_entry(\n                  Pathname.new(home)/".local/share/applications/whisper-dictate.desktop",\n                  bin/"whisper-dictate"', bump_step)
        self.assertIn("File.write(path.to_s", bump_step)
        self.assertIn("Icon=#{icon_path}", bump_step)
        self.assertIn("StartupWMClass=whisper-dictate", bump_step)
        self.assertIn('raw.include?("Icon=#{icon_path}")', bump_step)
        self.assertIn('raw.include?("StartupWMClass=whisper-dictate")', bump_step)
        self.assertIn("def install_linux_app_icon(home)", bump_step)
        self.assertIn("def linux_app_icon_path(home)", bump_step)
        self.assertIn('quiet_system "gtk-update-icon-cache"', bump_step)
        self.assertNotIn('\n              system "gtk-update-icon-cache"', bump_step)
        self.assertNotIn("path.write <<~DESKTOP", bump_step)
        self.assertNotIn("DESKTOP\n              )", bump_step)
        self.assertIn("def autostart_enabled_line(raw)", bump_step)
        self.assertIn('assert_path_exists libexec/"ubuntu26.04/setup.sh"', bump_step)
        self.assertNotIn("archive/refs/tags", bump_step)
        self.assertNotIn('exec "#{libexec}/setup.sh"', bump_step)

    def test_chocolatey_package_template_installs_release_asset(self):
        nuspec = Path("packaging/windows/chocolatey/whisper-dictate.nuspec").read_text(
            encoding="utf-8"
        )
        install = Path(
            "packaging/windows/chocolatey/tools/chocolateyinstall.ps1"
        ).read_text(encoding="utf-8")
        uninstall = Path(
            "packaging/windows/chocolatey/tools/chocolateyuninstall.ps1"
        ).read_text(encoding="utf-8")

        self.assertIn("<id>whisper-dictate</id>", nuspec)
        self.assertIn("<version>__VERSION__</version>", nuspec)
        self.assertIn("__INSTALLER_URL__", install)
        self.assertIn("__INSTALLER_SHA256__", install)
        self.assertIn("Install-ChocolateyPackage @packageArgs", install)
        self.assertIn("Uninstall-ChocolateyPackage @packageArgs", uninstall)

    def test_release_builds_and_optionally_pushes_chocolatey_package(self):
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            self.assertIn("- name: Build Chocolatey package", workflow, path.as_posix())
            self.assertIn("choco pack", workflow, path.as_posix())
            self.assertIn("Output/*.nupkg", workflow, path.as_posix())
            self.assertIn("packages: write", workflow, path.as_posix())
            self.assertIn("Publish Chocolatey package to GitHub Packages NuGet", workflow, path.as_posix())
            self.assertIn("nuget.pkg.github.com/${{ github.repository_owner }}", workflow, path.as_posix())
            self.assertIn("dotnet nuget push", workflow, path.as_posix())
            self.assertIn("Publish public Chocolatey feed to GitHub Pages", workflow, path.as_posix())
            self.assertIn(".\\scripts\\windows\\publish-chocolatey-feed.ps1 -PackagePath", workflow, path.as_posix())
            self.assertIn("CHOCOLATEY_NUGET_SOURCE", workflow, path.as_posix())
            self.assertIn("CHOCOLATEY_NUGET_API_KEY", workflow, path.as_posix())
            self.assertIn("choco push", workflow, path.as_posix())
            self.assertIn("packaging/windows/", workflow, path.as_posix())

    def test_public_chocolatey_feed_script_publishes_static_github_pages_feed(self):
        script = Path("scripts/windows/publish-chocolatey-feed.ps1").read_text(
            encoding="utf-8"
        )

        self.assertIn("dotnet tool install --tool-path $toolPath Sleet", script)
        self.assertIn("--version \"7.*\"", script)
        self.assertIn("$FeedBranch = \"gh-pages\"", script)
        self.assertIn("$FeedPath = \"chocolatey\"", script)
        self.assertIn("github.io", script)
        self.assertIn("Join-Path $feedRoot \".nojekyll\"", script)
        self.assertIn("git -C $feedRoot add .nojekyll", script)
        self.assertIn("sleet init --config $configPath --source githubPages", script)
        self.assertIn("sleet push --config $configPath --source githubPages --force", script)
        self.assertIn("sleet validate --config $configPath --source githubPages", script)
        self.assertIn("git -C $feedRoot push origin \"HEAD:$FeedBranch\"", script)

    def test_readme_documents_public_chocolatey_source(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        front_page = readme.split("## Ubuntu 24.04 / 26.04", 1)[0]

        self.assertIn("Windows 10 / 11 | Chocolatey or Rust UI installer", front_page)
        self.assertIn("Windows quick install with Chocolatey CLI 2.x", front_page)
        self.assertIn(
            "https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json",
            front_page,
        )
        self.assertIn("Install via public Chocolatey source", readme)
        self.assertIn(
            "https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json",
            readme,
        )
        self.assertIn("public and does not require a GitHub account or token", readme)
        self.assertIn("Chocolatey CLI 2.x NuGet v3 feed support", readme)
        self.assertIn("choco source add -n=whisper-dictate", readme)
        self.assertIn("choco source list", readme)
        self.assertIn("choco install whisper-dictate --source=whisper-dictate -y", readme)
        self.assertIn("choco upgrade whisper-dictate --source=whisper-dictate -y", readme)
        self.assertIn("choco pin add -n=whisper-dictate", readme)
        self.assertIn("choco uninstall whisper-dictate -y", readme)
        self.assertIn("choco --version", readme)
        self.assertIn(r"winget install --manifest .\whisper-dictate\packaging\windows\winget", readme)
        self.assertIn("nuget.pkg.github.com/FactusConsulting/index.json", readme)
        self.assertIn("CHOCOLATEY_NUGET_SOURCE", readme)
        self.assertIn("CHOCOLATEY_NUGET_API_KEY", readme)

    def test_winget_manifests_live_with_windows_packaging(self):
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            self.assertIn(r'$manifestDir = "packaging\windows\winget"', workflow)
            self.assertIn("packaging/windows/winget", workflow)
            self.assertIn("git add packaging/windows/winget/", workflow)
            self.assertNotIn("New-Item -ItemType Directory -Force manifests", workflow)

        for name in (
            "FactusConsulting.WhisperDictate.yaml",
            "FactusConsulting.WhisperDictate.locale.en-US.yaml",
            "FactusConsulting.WhisperDictate.installer.yaml",
        ):
            self.assertTrue(Path("packaging/windows/winget", name).is_file(), name)
        self.assertFalse(Path("manifests").exists())

    def test_rust_workspace_uses_single_root_lockfile(self):
        self.assertTrue(Path("Cargo.lock").is_file())
        self.assertFalse(Path("src/rust/whisper-dictate-app/Cargo.lock").exists())

    def test_sonar_uses_supported_python_version(self):
        sonar = Path("sonar-project.properties").read_text(encoding="utf-8")

        self.assertIn("sonar.projectKey=FactusConsulting_whisper-dictate", sonar)
        self.assertIn("sonar.python.version=3.12", sonar)

    def test_root_flake_delegates_to_nix_flake_logic(self):
        root_flake = Path("flake.nix").read_text(encoding="utf-8")
        nix_flake = Path("nix/flake.nix").read_text(encoding="utf-8")
        package = Path("nix/package.nix").read_text(encoding="utf-8")

        self.assertIn("outputs = inputs: import ./nix/flake.nix inputs;", root_flake)
        self.assertIn("pkgs.callPackage ./package.nix { src = self; }", nix_flake)
        self.assertIn("import ./module.nix", nix_flake)
        self.assertIn("Used by nix/flake.nix", package)
        self.assertIn('$out/lib/whisper-dictate/src/python', package)

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

    def test_smoke_workflow_sets_pythonpath_with_cross_shell_env(self):
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        self.assertNotIn("PYTHONPATH=src/python python", workflow)
        self.assertIn(
            "env:\n          PYTHONPATH: src/python\n        run: python -m whisper_dictate.runtime --help",
            workflow,
        )
        self.assertIn(
            "env:\n          PYTHONPATH: src/python\n        run: python -m whisper_dictate.runtime --doctor",
            workflow,
        )

    def test_dependabot_scans_only_requirements_directory_for_pip(self):
        config = Path(".github/dependabot.yml").read_text(encoding="utf-8")

        self.assertIn('package-ecosystem: "pip"', config)
        self.assertIn('directory: "/requirements"', config)
        self.assertNotIn('directory: "/"', config)

    def test_write_permissions_are_job_scoped(self):
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            pre_jobs = workflow.split("\njobs:", 1)[0]
            self.assertNotIn("contents: write", pre_jobs, path.as_posix())
            self.assertIn("permissions:\n      contents: write", workflow, path.as_posix())

    def test_workflow_main_pushes_retry_after_rebase(self):
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            self.assertIn("for attempt in 1 2 3; do", workflow, path.as_posix())
            self.assertIn('echo "push attempt $attempt"', workflow, path.as_posix())
            self.assertIn("git rebase origin/main", workflow, path.as_posix())

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
