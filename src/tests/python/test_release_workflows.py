from helpers import (
    Path,
    unittest,
)
import json
import re
import tomllib

class RustReleaseWorkflowTests(unittest.TestCase):
    def test_release_uploads_linux_rust_ui_binary(self):
        workflow = Path(".github/workflows/release.yml").read_text(encoding="utf-8")

        self.assertIn("cargo build --manifest-path src/rust/Cargo.toml --target-dir target --release -p whisper-dictate-app", workflow)
        self.assertIn("whisper-dictate-linux-rust-ui-${VERSION}", workflow)
        self.assertIn('install -m 0755 target/release/whisper-dictate "$d/whisper-dictate"', workflow)
        self.assertIn('INCLUDE_RUST_UI=1 mkbundle "whisper-dictate-linux-${VERSION}.zip"', workflow)
        self.assertIn('cp -r requirements "$d/"', workflow)
        self.assertIn('cp assets/whisper-dictate-logo.svg "$d/assets/"', workflow)
        self.assertIn("scripts/linux/install-rust-ui.sh", workflow)
        self.assertIn('cp packaging/linux/ubuntu26.04/setup.sh "$d/packaging/linux/ubuntu26.04/"', workflow)
        self.assertIn("bash -n scripts/linux/install-rust-ui.sh", workflow)
        self.assertIn("bash -n packaging/linux/ubuntu26.04/setup.sh", workflow)

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
        self.assertIn('assert_path_exists libexec/"packaging/linux/ubuntu26.04/setup.sh"', bump_step)
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
        self.assertIn("Join-Path $env:LOCALAPPDATA 'Programs\\WhisperDictate'", install)
        self.assertIn("Uninstall-BinFile -Name $packageName", install)
        self.assertIn("Install-BinFile -Name $packageName -Path $exePath", install)
        self.assertIn("Uninstall-ChocolateyPackage @packageArgs", uninstall)
        self.assertIn("Uninstall-BinFile -Name $packageName", uninstall)

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
        # winget manifests are version-controlled under packaging/windows/winget
        # and updated via PR — NOT generated or committed by CI (main is
        # protected: PR + CI only, so CI must not push to it).
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("Generate winget manifests", workflow, path.as_posix())
            self.assertNotIn("git add packaging/windows/winget/", workflow, path.as_posix())
            self.assertNotIn("New-Item -ItemType Directory -Force manifests", workflow)

        for name in (
            "FactusConsulting.WhisperDictate.yaml",
            "FactusConsulting.WhisperDictate.locale.en-US.yaml",
            "FactusConsulting.WhisperDictate.installer.yaml",
        ):
            self.assertTrue(Path("packaging/windows/winget", name).is_file(), name)
        self.assertFalse(Path("manifests").exists())

    def test_rust_crate_is_flat_single_crate_under_src_rust(self):
        # The Rust code lives directly under src/rust as a single crate — no
        # workspace wrapper and no nested per-crate subdirectory.
        manifest = tomllib.loads(Path("src/rust/Cargo.toml").read_text(encoding="utf-8"))

        self.assertIn("package", manifest)
        self.assertNotIn("workspace", manifest)
        self.assertEqual(manifest["package"]["name"], "whisper-dictate-app")
        self.assertTrue(Path("src/rust/Cargo.lock").is_file())
        self.assertFalse(Path("src/rust/whisper-dictate-app").exists())
        self.assertFalse(Path("Cargo.toml").exists())
        self.assertFalse(Path("Cargo.lock").exists())

    def test_rust_workspace_disables_incremental_cache_for_windows_stability(self):
        workspace = tomllib.loads(Path("src/rust/Cargo.toml").read_text(encoding="utf-8"))

        self.assertFalse(workspace["profile"]["dev"]["incremental"])
        self.assertFalse(workspace["profile"]["test"]["incremental"])

    def test_rust_toolchain_is_pinned_for_reproducible_ci(self):
        # An unpinned "stable" drifts rustfmt/clippy between releases, so "green
        # locally" stops guaranteeing "green in CI" (we hit exactly that: a newer
        # CI rustfmt re-wrapped a chain the local one left inline). The
        # `rust-toolchain.toml` pin gives both CI (via dtolnay/rust-toolchain,
        # which reads it when no `toolchain:` input is set) and local rustup one
        # concrete version. Bump it deliberately — never back to a floating channel.
        toolchain = tomllib.loads(
            Path("rust-toolchain.toml").read_text(encoding="utf-8")
        )["toolchain"]

        channel = toolchain["channel"]
        parts = channel.split(".")
        self.assertEqual(
            len(parts), 3, f"channel must be a concrete x.y.z pin, got {channel!r}"
        )
        self.assertTrue(
            all(p.isdigit() for p in parts),
            f"channel must be a numeric version, got {channel!r}",
        )

        # Both CI gates (fmt + clippy) must run on the pinned toolchain.
        self.assertIn("rustfmt", toolchain["components"])
        self.assertIn("clippy", toolchain["components"])

        # No dtolnay/rust-toolchain step may pass a `toolchain:` input — it would
        # override rust-toolchain.toml and re-introduce drift. Scope the check to
        # each action's own step block (up to the next step), not the whole file,
        # so an unrelated `toolchain:` elsewhere can't trip the guard. Scan both
        # extensions so a future `.yaml` workflow can't slip past.
        workflows = [
            *Path(".github/workflows").glob("*.yml"),
            *Path(".github/workflows").glob("*.yaml"),
        ]
        for path in workflows:
            text = path.read_text(encoding="utf-8")
            for after in text.split("dtolnay/rust-toolchain")[1:]:
                step_block = re.split(r"\n\s*- ", after, maxsplit=1)[0]
                self.assertNotIn("toolchain:", step_block, path.as_posix())

    def test_vscode_rust_analyzer_links_moved_workspace(self):
        settings = Path(".vscode/settings.json").read_text(encoding="utf-8")
        gitignore = Path(".gitignore").read_text(encoding="utf-8")

        self.assertIn('"rust-analyzer.linkedProjects"', settings)
        self.assertIn('"src/rust/Cargo.toml"', settings)
        self.assertIn("!.vscode/settings.json", gitignore)

    def test_sonar_uses_supported_python_version(self):
        sonar = Path("sonar-project.properties").read_text(encoding="utf-8")
        workflow = Path(".github/workflows/sonar.yml").read_text(encoding="utf-8")
        test_workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        self.assertIn("sonar.projectKey=FactusConsulting_whisper-dictate", sonar)
        self.assertIn("sonar.python.version=3.12", sonar)
        self.assertIn("sonar.rust.cargo.manifestPaths=src/rust/Cargo.toml", sonar)
        # Rust is CI-analysed (not AA-eligible); Clippy lints are imported from a
        # JSON report rather than re-run by the analyzer.
        self.assertIn("sonar.rust.clippy.enabled=false", sonar)
        self.assertIn("sonar.rust.clippyReport.reportPaths=clippy-report.json", sonar)
        # sources and tests must be disjoint or the scanner fails ("indexed twice");
        # the test dirs live under src/, so sources lists the main dirs explicitly.
        # The flat Rust crate lives at src/rust, with its tests carved out via
        # sonar.exclusions so the nested src/rust/tests isn't indexed twice.
        self.assertIn(
            "sonar.sources=src/python/whisper_dictate,src/rust,scripts,packaging,nix",
            sonar,
        )
        self.assertNotIn("sonar.sources=src,", sonar)
        self.assertIn("src/rust/tests/**", sonar)
        self.assertIn("components: clippy", workflow)
        self.assertIn("cargo clippy --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app --all-targets --all-features --message-format=json > clippy-report.json", workflow)
        self.assertRegex(workflow, r"uses:\s*SonarSource/sonarqube-scan-action@v\d+")
        self.assertIn("SONAR_TOKEN: ${{ secrets.SONAR_TOKEN }}", workflow)
        self.assertIn("components: clippy", test_workflow)
        self.assertIn("cargo clippy --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app --all-targets --all-features -- -D warnings", test_workflow)

    def test_sonar_imports_python_and_rust_coverage(self):
        sonar = Path("sonar-project.properties").read_text(encoding="utf-8")
        workflow = Path(".github/workflows/sonar.yml").read_text(encoding="utf-8")

        # Coverage report paths are wired into the Sonar properties.
        self.assertIn("sonar.python.coverage.reportPaths=coverage.xml", sonar)
        self.assertIn("sonar.rust.lcov.reportPaths=lcov.info", sonar)

        # sonar.yml must generate both reports before the scan runs.
        self.assertIn("components: clippy, llvm-tools-preview", workflow)
        self.assertIn("tool: cargo-llvm-cov", workflow)
        self.assertIn(
            'cargo llvm-cov --manifest-path src/rust/Cargo.toml -p whisper-dictate-app --all-features --lcov --output-path "$GITHUB_WORKSPACE/lcov.info"',
            workflow,
        )
        self.assertIn(
            "python -m coverage run --source=src/python/whisper_dictate -m pytest src/python/tests src/tests/python -q",
            workflow,
        )
        self.assertIn("python -m coverage xml -o coverage.xml", workflow)
        # LCOV `SF:` paths must be normalized to repo-root-relative or Sonar
        # cannot map them onto the indexed Rust sources.
        self.assertIn('sed -i "s#^SF:$(pwd)/#SF:#" lcov.info', workflow)

    def test_sonar_excludes_untestable_integration_from_coverage(self):
        # Code whose uncovered remainder is genuinely OS/IO/UI integration (not
        # unit-testable business logic) is kept out of the coverage metric — it
        # is still analysed for issues — so the metric reflects testable logic.
        sonar = Path("sonar-project.properties").read_text(encoding="utf-8")
        match = re.search(r"^sonar\.coverage\.exclusions=(.+)$", sonar, re.MULTILINE)
        self.assertIsNotNone(match, "sonar.coverage.exclusions must be set")
        patterns = {p.strip() for p in match.group(1).split(",")}
        expected = {
            # egui immediate-mode rendering
            "src/rust/ui/tabs/**",
            "src/rust/ui/widgets.rs",
            "src/rust/ui/previews.rs",
            "src/rust/ui/log_render.rs",
            "src/rust/ui/app.rs",
            "src/rust/ui.rs",
            # OS credential store, config-IO app state, subprocess, entrypoint
            "src/rust/ui/api_keys.rs",
            "src/rust/ui/secret_store.rs",
            "src/rust/ui/settings_state.rs",
            "src/rust/ui/tasks.rs",
            "src/rust/main.rs",
            "src/python/whisper_dictate/vp_keys.py",
            "src/python/whisper_dictate/vp_rust.py",
            "src/python/whisper_dictate/runtime.py",
            "src/python/whisper_dictate/vp_dictate.py",
            # developer/benchmark scripts
            "scripts/dev/**",
            "scripts/benchmark/**",
        }
        self.assertTrue(
            expected.issubset(patterns),
            f"sonar.coverage.exclusions missing: {sorted(expected - patterns)}",
        )

    def test_root_flake_delegates_to_nix_flake_logic(self):
        root_flake = Path("flake.nix").read_text(encoding="utf-8")
        nix_flake = Path("nix/flake.nix").read_text(encoding="utf-8")
        package = Path("nix/package.nix").read_text(encoding="utf-8")

        self.assertIn("outputs = inputs: import ./nix/flake.nix inputs;", root_flake)
        self.assertIn("pkgs.callPackage ./package.nix { src = self; }", nix_flake)
        self.assertIn("import ./module.nix", nix_flake)
        self.assertIn("Used by nix/flake.nix", package)
        self.assertIn('$out/lib/whisper-dictate/src/python', package)

    def test_dictionary_example_lives_under_docs_examples(self):
        self.assertFalse(Path("dictionary.example.json").exists())
        self.assertTrue(Path("docs/examples/dictionary.example.json").is_file())

        inno = Path("packaging/windows/inno/whisper-dictate.iss").read_text(
            encoding="utf-8"
        )
        local_installer = Path("scripts/windows/build-installer.ps1").read_text(
            encoding="utf-8"
        )
        workflows = "\n".join(
            path.read_text(encoding="utf-8")
            for path in (
                Path(".github/workflows/release.yml"),
                Path(".github/workflows/windows-installer.yml"),
            )
        )

        self.assertIn(
            r'Source: "..\..\..\docs\examples\dictionary.example.json"; DestDir: "{app}\docs\examples"',
            inno,
        )
        self.assertNotIn("dictionary.example.json') -Destination $bundle", local_installer)
        self.assertNotIn("Copy-Item dictionary.example.json", workflows)
        self.assertNotIn("[ -f dictionary.example.json ]", workflows)

    def test_python_tests_live_under_source_test_roots(self):
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")
        readme = Path("README.md").read_text(encoding="utf-8")
        technical = Path("docs/TECHNICAL.md").read_text(encoding="utf-8")
        test_command = "python -m pytest src/python/tests src/tests/python -q"

        self.assertFalse(Path("tests").exists())
        self.assertTrue(Path("src/python/tests/helpers.py").is_file())
        self.assertTrue(Path("src/python/tests/test_audio.py").is_file())
        self.assertTrue(Path("src/tests/python/test_release_workflows.py").is_file())
        self.assertIn(test_command, workflow)
        self.assertIn(test_command, readme)
        self.assertIn(test_command, technical)
        self.assertNotIn("python -m pytest tests -q", workflow)
        self.assertNotIn("python -m pytest src/tests/python -q", workflow)

    def test_workflows_use_node24_checkout_action(self):
        # Guard the Node24 floor: every actions/checkout must be v5+ (reject the
        # deprecated Node20 v4 and older). Robust to Dependabot bumping the major.
        # Scan both extensions so a future `.yaml` workflow can't slip past.
        workflows = [
            *Path(".github/workflows").glob("*.yml"),
            *Path(".github/workflows").glob("*.yaml"),
        ]
        majors = []
        for path in workflows:
            workflow = path.read_text(encoding="utf-8")
            for major in re.findall(r"uses:\s*actions/checkout@v(\d+)", workflow):
                majors.append((path.as_posix(), int(major)))
        self.assertTrue(majors, "no workflow uses actions/checkout")
        for path, major in majors:
            self.assertGreaterEqual(major, 5, f"{path} uses checkout older than v5 (Node20)")

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

    def test_dependabot_covers_pip_actions_and_cargo(self):
        config = Path(".github/dependabot.yml").read_text(encoding="utf-8")
        # Check per-ecosystem blocks so the directory is paired with the right
        # ecosystem (not just present somewhere in the file).
        blocks = config.split("- package-ecosystem:")
        pip = next(b for b in blocks if '"pip"' in b)
        self.assertIn('directory: "/requirements"', pip)  # pip scoped, not repo root
        cargo = next(b for b in blocks if '"cargo"' in b)
        self.assertIn('directory: "/src/rust"', cargo)
        self.assertTrue(any('"github-actions"' in b for b in blocks))

    def test_ci_caches_rust_and_cancels_superseded_runs(self):
        test_wf = Path(".github/workflows/test.yml").read_text(encoding="utf-8")
        sonar_wf = Path(".github/workflows/sonar.yml").read_text(encoding="utf-8")
        devcontainer_wf = Path(".github/workflows/devcontainer.yml").read_text(encoding="utf-8")

        # Rust builds (registry + target) are cached on the per-PR jobs so they
        # don't recompile the whole egui/TLS tree from scratch every run.
        self.assertIn("Swatinem/rust-cache@", test_wf)
        self.assertIn("Swatinem/rust-cache@", sonar_wf)

        # Superseded PR runs are cancelled — but never main/release runs (the
        # group is unique for non-PR events), so the release gate (test.yml via
        # workflow_call on a tag) is neither cancelled nor serialized.
        for wf in (test_wf, sonar_wf, devcontainer_wf):
            self.assertIn("concurrency:", wf)
            self.assertIn(
                "cancel-in-progress: ${{ github.event_name == 'pull_request' }}", wf
            )

    def test_configuration_doc_covers_every_schema_setting(self):
        # docs/CONFIGURATION.md must document every setting in the schema (the
        # single source of truth), so the reference can't silently drift.
        schema = json.loads(
            Path("src/python/whisper_dictate/settings_schema.json").read_text(encoding="utf-8")
        )
        doc = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")
        # Match the backticked form so a stray mention in prose/examples doesn't
        # count as "documented" — settings are always backticked in the tables.
        missing = sorted(
            s["env"] for s in schema["settings"] if f"`{s['env']}`" not in doc
        )
        self.assertEqual([], missing, f"docs/CONFIGURATION.md is missing: {missing}")

    def test_configuration_cheat_sheet_keeps_ui_tab_column_with_all_tabs(self):
        # Guards only that the cheat sheet keeps its "UI tab" column and that all
        # five tab labels appear at least once (not a full per-setting mapping).
        # Tab labels mirror src/rust/ui/text.rs (Speech/Quality/Dictionary/Output/Post).
        doc = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")
        self.assertIn("| Knob | UI tab | Env var | CLI flag |", doc)
        for tab in ("Speech", "Quality", "Dictionary", "Output", "Post"):
            self.assertIn(f"| {tab} |", doc, f"cheat sheet has no {tab} tab cell")

    def test_write_permissions_are_job_scoped(self):
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            pre_jobs = workflow.split("\njobs:", 1)[0]
            self.assertNotIn("contents: write", pre_jobs, path.as_posix())
            self.assertIn("permissions:\n      contents: write", workflow, path.as_posix())

    def test_release_workflows_do_not_push_version_bumps_to_main(self):
        # main is protected (PR + CI only): release CI must not push to it.
        # nix/package.nix is bumped in the pre-release version PR (with VERSION);
        # winget manifests via a separate PR. The Homebrew tap push targets a
        # different repo (cd tap) and is unaffected.
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("Bump nix/package.nix version", workflow, path.as_posix())
            self.assertNotIn("Generate winget manifests", workflow, path.as_posix())
            self.assertNotIn("Commit updated manifests to main", workflow, path.as_posix())
            self.assertNotIn("for attempt in 1 2 3", workflow, path.as_posix())
            self.assertNotIn("git push origin main", workflow, path.as_posix())

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

    def test_release_is_gated_on_the_full_test_suite(self):
        test_workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")
        release = Path(".github/workflows/release.yml").read_text(encoding="utf-8")

        # test.yml must be reusable so the release runs the exact same checks
        # instead of duplicating them.
        self.assertIn("workflow_call:", test_workflow)

        # The release calls the reusable suite and blocks on it; the dependent
        # windows-installer job is gated transitively via needs: release.
        self.assertIn("  tests:\n    uses: ./.github/workflows/test.yml", release)
        self.assertIn("  release:\n    needs: tests\n", release)
        self.assertIn("  windows-installer:\n    needs: release\n", release)

        # Manual (workflow_dispatch) re-releases must gate on the *tag* being
        # shipped, not the dispatch branch: the reusable suite checks out a ref
        # input in its jobs, and the release passes the resolved tag into it.
        self.assertIn("ref: ${{ inputs.ref }}", test_workflow)
        self.assertIn("ref: ${{ github.event.inputs.tag || github.ref_name }}", release)

    def test_devcontainer_mirrors_ci_linux_build_deps(self):
        # The dev container must apt-install every Linux egui/build dep that the
        # CI rust job does, so "green in the container" == "green in CI". Parse
        # CI's list dynamically so adding a dep there forces it here too.
        dockerfile = Path(".devcontainer/Dockerfile").read_text(encoding="utf-8")
        test_workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        ci_deps = set(re.findall(r"\b(?:pkg-config|lib[a-z0-9-]+-dev)\b", test_workflow))
        self.assertIn("libxkbcommon-dev", ci_deps)  # sanity: the list was found
        # Token-precise (not a loose substring) so a dep can't "match" as part
        # of a longer package name.
        dockerfile_tokens = set(dockerfile.split())
        missing = sorted(dep for dep in ci_deps if dep not in dockerfile_tokens)
        self.assertEqual([], missing, f"dev container missing CI build deps: {missing}")

        # Python stack mirrors the CI unit/lint jobs; Rust comes from rustup with
        # the channel pinned by rust-toolchain.toml (no inline default toolchain).
        self.assertIn("python3-venv", dockerfile)
        self.assertIn("pytest", dockerfile)
        self.assertIn("numpy", dockerfile)
        self.assertIn("--default-toolchain none", dockerfile)
        self.assertTrue(Path("rust-toolchain.toml").is_file())

    def test_devcontainer_json_builds_the_dockerfile(self):
        config = json.loads(
            Path(".devcontainer/devcontainer.json").read_text(encoding="utf-8")
        )
        self.assertEqual(config["build"]["dockerfile"], "Dockerfile")
        self.assertEqual(config["remoteUser"], "vscode")
        # postCreate installs the pinned toolchain once the repo is mounted.
        self.assertIn("rustup show", config["postCreateCommand"])

    def test_devcontainer_ci_builds_and_runs_the_dev_loop(self):
        workflow = Path(".github/workflows/devcontainer.yml").read_text(encoding="utf-8")
        self.assertIn("devcontainers/ci@", workflow)
        # The container's Rust loop must match the CI rust job verbatim (incl.
        # --target-dir target) so the two can't drift.
        self.assertIn(
            "cargo clippy --manifest-path src/rust/Cargo.toml --target-dir target "
            "-p whisper-dictate-app --all-targets --all-features -- -D warnings",
            workflow,
        )
        self.assertIn(
            "cargo test --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app",
            workflow,
        )
        self.assertIn("python -m pytest src/python/tests src/tests/python", workflow)
