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


class WindowsDocsAndPackagingRegressionTests(unittest.TestCase):
    def test_config_maps_audio_ducking_and_cloud_redaction(self):
        config = Path("src/python/whisper_dictate/settings_schema.json").read_text(encoding="utf-8")
        rust_config = rust_config_source()
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
        cli = Path("src/rust/cli.rs").read_text(encoding="utf-8")
        main = Path("src/rust/main.rs").read_text(encoding="utf-8")
        runtime = Path("src/rust/runtime.rs").read_text(encoding="utf-8")

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
        runtime = Path("src/rust/runtime.rs").read_text(encoding="utf-8")

        self.assertIn('VOICEPI_RUST_OWNS_DESKTOP', script)
        self.assertIn("fn linux_desktop_exec_command() -> String", runtime)
        self.assertIn('format!("{} ui", desktop_exec_token(&exe))', runtime)
        self.assertIn('Name=Whisper Dictate', runtime)
        self.assertIn('.local/share/applications', runtime)
        self.assertIn('.config/autostart', runtime)
        self.assertIn('gtk-launch', runtime)
        self.assertIn('setsid', runtime)
        # The --lang value is now parameterised via $WD_LANG (default da).
        self.assertIn('Terminal-runtime: whisper-dictate run --key shift_r+ctrl_r --lang ${WD_LANG}', script)
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
        install = Path("docs/INSTALLATION.md").read_text(encoding="utf-8")
        config = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")
        technical = Path("docs/TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn(
            "runs the Rust UI and starts the Python worker hidden underneath it",
            readme.replace("\n", " "),
        )
        self.assertIn("whisper-dictate run --key ctrl_r --lang da", readme)
        self.assertIn(r"whisper-dictate.exe run --key ctrl_r --lang da --device cuda", readme)
        self.assertIn(
            "runs the Rust UI and starts the Python worker hidden underneath it",
            install.replace("\n", " "),
        )
        self.assertIn("whisper-dictate.exe\" run --key ctrl_r --lang da --model large-v3 --device cuda", config)
        self.assertIn(r"whisper-dictate.exe run --key ctrl_r --lang da", config)
        self.assertIn("Rust UI is the installer Start-menu", technical)
        self.assertIn("no compatibility script is installed", technical)
        self.assertNotIn("whisper-dictate Terminal", readme)
        self.assertNotIn("whisper-dictate Debug Terminal", readme)
        self.assertNotIn("Current primary path is the installed PySide/PowerShell UI", technical)

    def test_docs_describe_groq_as_explicit_opt_in_without_storing_keys(self):
        config = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")

        self.assertIn("https://api.groq.com/openai/v1", config)
        self.assertIn("whisper-large-v3-turbo", config)
        self.assertIn("GROQ_API_KEY", config)
        self.assertIn("VOICEPI_POST_API_KEY", config)
        self.assertIn("Cloud STT provider", config)
        self.assertIn("post_processor", config)
        self.assertIn("OS credential store", config)

    def test_docs_describe_one_command_ubuntu_setup_and_launcher_start(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        install = Path("docs/INSTALLATION.md").read_text(encoding="utf-8")
        config = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")

        for doc in (readme, install, config):
            self.assertIn("whisper-dictate setup-ubuntu", doc)
        for doc in (install, config):
            self.assertIn("Whisper Dictate", doc)
            self.assertIn("whisper-dictate ui", doc)
        self.assertIn("Then press **Start** in the Dictation tab", install)

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
        cargo = Path("src/rust/Cargo.toml").read_text(encoding="utf-8")
        build = Path("src/rust/build.rs").read_text(encoding="utf-8")

        self.assertIn("winresource", cargo)
        self.assertIn("CARGO_CFG_TARGET_OS", build)
        self.assertIn('"windows"', build)
        self.assertIn("../../assets/whisper-dictate.ico", build)
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

    def test_installer_and_nix_ship_data_subpackage(self):
        # The anti-hallucination pattern JSON lives in the data/ subpackage and is
        # loaded at import via importlib.resources. The Inno *.py / *.json globs
        # are NOT recursive and the nix *.py loop is flat, so each needs an
        # explicit entry or the data file would be missing from the installed app
        # (a silent ImportError at startup).
        installer = Path("packaging/windows/inno/whisper-dictate.iss").read_text(encoding="utf-8")
        self.assertIn(
            r'Source: "..\..\..\src\python\whisper_dictate\data\*"; DestDir: "{app}\src\python\whisper_dictate\data"',
            installer,
        )
        nix = Path("nix/package.nix").read_text(encoding="utf-8")
        self.assertIn("src/python/whisper_dictate/data/hallucination_patterns.json", nix)
        # The data file must actually exist where the packaging entries point.
        self.assertTrue(
            Path("src/python/whisper_dictate/data/hallucination_patterns.json").exists())

    def test_packaging_ships_benchmark_corpus_manifest(self):
        # The "Run benchmark" button resolves <app-root>/benchmark/corpus.json, so
        # the manifest must be shipped by every bundle for the button to work out
        # of the box on a fresh install. The user-local audio recordings are
        # gitignored and intentionally NOT shipped (the worker falls back to the
        # per-user appdata audio dir for those).
        installer = Path("packaging/windows/inno/whisper-dictate.iss").read_text(encoding="utf-8")
        self.assertIn(
            r'Source: "..\..\..\benchmark\corpus.json"; DestDir: "{app}\benchmark"',
            installer,
        )
        # The audio dir must never be shipped (gitignored, user-local).
        self.assertNotIn(r"benchmark\audio", installer)

        zip_workflow = Path(".github/workflows/windows-installer-build.yml").read_text(encoding="utf-8")
        self.assertIn(
            r'Copy-Item benchmark\corpus.json (Join-Path $bundle "benchmark")',
            zip_workflow,
        )

        local_script = Path("scripts/windows/build-installer.ps1").read_text(encoding="utf-8")
        self.assertIn(r"benchmark\corpus.json", local_script)

        linux_release = Path(".github/workflows/release.yml").read_text(encoding="utf-8")
        self.assertIn("cp benchmark/corpus.json \"$d/benchmark/\"", linux_release)

        # The shipped manifest must actually exist where every entry points.
        self.assertTrue(Path("benchmark/corpus.json").exists())

    def test_release_has_install_smoke_gate_a(self):
        # Gate A (docs/RELEASING.md "Test the RC" step 1) is the automated
        # install-smoke job in release.yml. This guard pins its key contract so
        # the job — and each assertion that makes it meaningful — cannot be
        # silently dropped or hollowed out: it must silently install the
        # published setup .exe, assert the full installed layout (incl. the #226
        # data subpackage + a VERSION matching the tag), launch the Rust
        # controller headless (exit 0, no window), and run the no-model worker
        # audio query modes cleanly. It must run for BOTH finals and prereleases
        # and depend on the release + installer jobs.
        workflow = Path(".github/workflows/release.yml").read_text(encoding="utf-8")

        # The job exists, depends on both upstream jobs, and runs on Windows.
        self.assertIn("install-smoke:", workflow)
        self.assertIn("needs: [release, windows-installer]", workflow)
        self.assertIn("runs-on: windows-2025", workflow)

        # Downloads the just-published setup .exe by its asset pattern.
        self.assertIn(
            "--pattern 'whisper-dictate-windows-setup-*.exe'", workflow)

        # Silent Inno install with the documented flags.
        for flag in ("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"):
            self.assertIn(flag, workflow)

        # Installed-layout assertions (the load-bearing regression guards).
        self.assertIn(r"Programs\WhisperDictate", workflow)
        self.assertIn(r"src\python\whisper_dictate\runtime.py", workflow)
        self.assertIn(r"benchmark\corpus.json", workflow)
        self.assertIn(
            r"src\python\whisper_dictate\data\hallucination_patterns.json",
            workflow)
        # VERSION must be asserted equal to the tag's version.
        self.assertIn("Installed VERSION", workflow)

        # Rust controller headless smoke: a non-UI entrypoint, exit-code checked.
        self.assertIn("whisper-dictate.exe --version", workflow)

        # Worker no-model audio query modes, minimal deps only (no heavy ML).
        self.assertIn("--test-audio-device", workflow)
        self.assertIn("--list-audio-devices", workflow)
        self.assertIn("sounddevice", workflow)
        for excluded in ("faster-whisper", "torch", "ctranslate2"):
            # The smoke venv must NOT pip-install the heavy model stack.
            self.assertNotIn(f"pip install --quiet '{excluded}", workflow)

        # Fix 1 (Copilot review): asset-existence check before download so a
        # transient network/API error cannot silently skip the gate. The job must
        # query the release asset list first; only a count of 0 legitimately
        # skips (found=false). If the asset exists, download failure is hard.
        self.assertIn(
            'select(test("whisper-dictate-windows-setup"))] | length',
            workflow,
        )

        # Fix 3 (Copilot review, CRITICAL): PYTHONPATH must point at the
        # installed worker package so both worker invocations can import
        # whisper_dictate (it lives at <app-root>\src\python in the installed
        # layout, not on the venv's site-packages).
        self.assertIn(r"src\python'", workflow)
        self.assertIn("PYTHONPATH", workflow)

        # Fix 2 (Copilot review): --list-audio-devices exit code must be
        # captured and asserted to be 0 or 1; any other code fails the gate.
        self.assertIn("ldCode", workflow)
        self.assertIn("-notin @(0, 1)", workflow)

    def test_windows_installer_workflows_build_rust_ui_before_inno(self):
        # The installer build steps live in the single reusable workflow shared by
        # release.yml and windows-installer.yml.
        workflow = Path(".github/workflows/windows-installer-build.yml").read_text(encoding="utf-8")
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
        releasing = Path("docs/RELEASING.md").read_text(encoding="utf-8")

        self.assertIn("function Get-CrateVersion", script)
        self.assertIn("src\\rust\\Cargo.toml", script)
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
        self.assertIn("<version>+local.<timestamp>.g<sha>.dirty", releasing)
        self.assertIn("<major>.<minor>.<patch>.1", releasing)

    def test_windows_zip_packages_are_built_on_windows_with_rust_exe(self):
        # The portable-ZIP build lives in the single reusable installer-build
        # workflow shared by release.yml and windows-installer.yml.
        workflow = Path(".github/workflows/windows-installer-build.yml").read_text(encoding="utf-8")

        self.assertIn("Build Windows ZIP packages", workflow)
        self.assertIn("whisper-dictate-windows-$version.zip", workflow)
        self.assertIn("whisper-dictate-windows-setup-$version.exe", workflow)
        self.assertIn("Copy-Item target\\release\\whisper-dictate.exe", workflow)
        self.assertIn("Copy-Item assets\\whisper-dictate.ico", workflow)
        self.assertNotIn('Copy-Item requirements-cpu.txt (Join-Path $bundle "requirements.txt")', workflow)
        self.assertNotIn('"requirements.txt"', workflow)
        self.assertIn("Copy-Item requirements $bundle -Recurse", workflow)
        self.assertIn("Output/*.exe Output/*.zip Output/*.nupkg sha256sums.txt", workflow)
        # The portable ZIP ships only the Python worker package — never the
        # whole src tree (no src\rust source, already compiled into the exe,
        # and no test trees). Mirrors the Inno installer's [Files] list.
        self.assertIn("Copy-Item src\\python\\whisper_dictate", workflow)
        self.assertNotIn("Copy-Item src $bundle -Recurse", workflow)
        # Runner-generated bytecode caches are pruned so the ZIP is reproducible.
        self.assertIn("-Filter '__pycache__'", workflow)

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
        releasing = Path("docs/RELEASING.md").read_text(encoding="utf-8")
        agents = Path("AGENTS.md").read_text(encoding="utf-8")
        technical = Path("docs/TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("portable Windows ZIP bundle", releasing)
        self.assertIn("installer and portable ZIP are written to `Output\\`", releasing)
        self.assertIn("Output\\*.exe` and `Output\\*.zip", agents)
        self.assertIn("Output\\*.exe` and `Output\\*.zip", technical)

    def test_project_history_is_not_maintained_by_hand(self):
        workflow = Path(".github/workflows/release.yml").read_text(encoding="utf-8")

        # History is the generated release notes (git log between tags), not a
        # hand-maintained changelog — anywhere in the tree.
        self.assertFalse(Path("CHANGELOG.md").exists())
        self.assertFalse(Path("docs/CHANGELOG.md").exists())
        self.assertFalse(Path("docs/RELEASE_NOTES.md").exists())
        self.assertIn("git log --no-merges", workflow)
        self.assertNotIn("docs/RELEASE_NOTES.md", workflow)

    def test_runtime_reconfigures_windows_streams_to_utf8(self):
        with open("src/python/whisper_dictate/runtime.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn('reconfigure(encoding="utf-8", errors="replace")', script)

    def test_runtime_has_parakeet_min_duration_and_backend_metrics(self):
        # The live Dictate loop moved into vp_dictate.
        with open("src/python/whisper_dictate/vp_dictate.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("self.parakeet_min_seconds", script)
        self.assertIn("too short for Parakeet", script)
        self.assertIn('"stt_backend": self.stt_backend', script)

    def test_runtime_has_live_release_tail_padding(self):
        # The live Dictate loop moved into vp_dictate.
        with open("src/python/whisper_dictate/vp_dictate.py", encoding="utf-8") as f:
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
