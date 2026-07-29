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
        # The installer build (incl. the Chocolatey pack/publish steps) lives in
        # ONE place — the reusable windows-installer-build.yml — so assert the
        # step content there. release.yml and windows-installer.yml only need to
        # CALL it; that wiring is checked by
        # test_installer_build_is_single_reusable_workflow.
        path = Path(".github/workflows/windows-installer-build.yml")
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

    def test_installer_build_is_single_reusable_workflow(self):
        # SINGLE SOURCE OF TRUTH guard: the Windows installer build steps must
        # live in exactly one reusable workflow, and BOTH the release pipeline and
        # the manual rebuild must call it via `uses:`. This is the regression
        # guard for the incident where the inline release job and the standalone
        # windows-installer.yml drifted (a prerelease Inno fix landed in only one).
        reusable = Path(".github/workflows/windows-installer-build.yml").read_text(
            encoding="utf-8"
        )
        release = Path(".github/workflows/release.yml").read_text(encoding="utf-8")
        standalone = Path(".github/workflows/windows-installer.yml").read_text(
            encoding="utf-8"
        )

        # The reusable workflow is callable and owns the build steps.
        self.assertIn("on:\n  workflow_call:", reusable)
        self.assertIn("- name: Build installers", reusable)
        self.assertIn("- name: Build Chocolatey package", reusable)
        self.assertIn("- name: Generate winget manifests", reusable)

        # Both callers defer to the one reusable workflow and pass the tag.
        for path, text in (
            (Path(".github/workflows/release.yml"), release),
            (Path(".github/workflows/windows-installer.yml"), standalone),
        ):
            self.assertIn(
                "uses: ./.github/workflows/windows-installer-build.yml",
                text,
                path.as_posix(),
            )
            self.assertIn("tag:", text, path.as_posix())

        # The build steps must NOT be duplicated back into either caller (that
        # duplication is exactly what caused the drift incident).
        for path, text in (
            (Path(".github/workflows/release.yml"), release),
            (Path(".github/workflows/windows-installer.yml"), standalone),
        ):
            self.assertNotIn("ISCC failed", text, path.as_posix())
            self.assertNotIn("choco pack", text, path.as_posix())
            self.assertNotIn("Generate winget manifests", text, path.as_posix())

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
        install = Path("docs/INSTALLATION.md").read_text(encoding="utf-8")

        self.assertIn("Windows 10 / 11 | Installer or Chocolatey", readme)
        self.assertIn(
            "https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json",
            readme,
        )
        self.assertIn("Windows quick install with Chocolatey CLI 2.x", install)
        self.assertIn(
            "https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json",
            install,
        )
        self.assertIn("public source does not require a GitHub account or token", install)
        self.assertIn("Chocolatey CLI 2.x NuGet v3 feed support", install)
        self.assertIn("choco source add -n=whisper-dictate", install)
        self.assertIn("choco source list", install)
        self.assertIn("choco install whisper-dictate --source=whisper-dictate -y", install)
        self.assertIn("choco upgrade whisper-dictate --source=whisper-dictate -y", install)
        self.assertIn("choco pin add -n=whisper-dictate", install)
        self.assertIn("choco uninstall whisper-dictate -y", install)
        self.assertIn("choco --version", install)
        self.assertIn(r"winget install --manifest .\winget", install)
        self.assertIn("whisper-dictate-winget-<version>.zip", install)
        self.assertIn("CHOCOLATEY_NUGET_SOURCE", install)
        self.assertIn("CHOCOLATEY_NUGET_API_KEY", install)

    def test_winget_manifests_are_templated_and_generated_in_release(self):
        # The packaging/windows/winget manifests are version-controlled TEMPLATES
        # (placeholders the release fills); CI generates the concrete manifests and
        # ships them as a release asset — it never commits them back to protected
        # main (PR + CI only, so CI must not push to it). Generation lives in the
        # single reusable installer-build workflow.
        path = Path(".github/workflows/windows-installer-build.yml")
        workflow = path.read_text(encoding="utf-8")
        # Generation happens, but the manifests are NOT committed/pushed.
        self.assertIn("Generate winget manifests", workflow, path.as_posix())
        self.assertIn("whisper-dictate-winget-$version.zip", workflow, path.as_posix())
        self.assertNotIn("git add packaging/windows/winget/", workflow, path.as_posix())
        self.assertNotIn("New-Item -ItemType Directory -Force manifests", workflow)
        # Winget is finals-only: the generate step is skipped on prerelease tags.
        self.assertIn(
            "if: steps.win-check.outputs.changed == 'true' && inputs.is_prerelease != 'true'",
            workflow,
            path.as_posix(),
        )
        # The release pipeline forwards the prerelease flag computed by the
        # release job into the reusable workflow, so RC tags ship no winget asset.
        release = Path(".github/workflows/release.yml").read_text(encoding="utf-8")
        self.assertIn(
            "is_prerelease: ${{ needs.release.outputs.is_prerelease }}", release
        )

        names = (
            "FactusConsulting.WhisperDictate.yaml",
            "FactusConsulting.WhisperDictate.locale.en-US.yaml",
            "FactusConsulting.WhisperDictate.installer.yaml",
        )
        for name in names:
            self.assertTrue(Path("packaging/windows/winget", name).is_file(), name)
        # Every manifest carries the version placeholder; the installer manifest
        # also templates the URL and SHA256 the release fills in.
        for name in names:
            text = Path("packaging/windows/winget", name).read_text(encoding="utf-8")
            self.assertIn("PackageVersion: __VERSION__", text, name)
        installer = Path(
            "packaging/windows/winget/FactusConsulting.WhisperDictate.installer.yaml"
        ).read_text(encoding="utf-8")
        self.assertIn("InstallerUrl: __INSTALLER_URL__", installer)
        self.assertIn("InstallerSha256: __INSTALLER_SHA256__", installer)
        self.assertIn("ReleaseDate: __RELEASE_DATE__", installer)
        self.assertFalse(Path("manifests").exists())

    def test_windows_installer_local_script_strips_rc_with_local_metadata(self):
        # Regression for the P2 #406 Codex finding: during an RC window the
        # default `Get-CrateVersion + Get-LocalBuildMetadata` path produces
        # `1.19.0-rc.1+local.<stamp>.<sha>` — the `-rc.N` is in the MIDDLE,
        # not at the end. The strip regex must match `-rc.N` followed by
        # either end-of-string OR `+local.` (build metadata), otherwise the
        # numeric/build-metadata patterns below fall through and the local
        # installer loop throws. Asserts the regex carries the lookahead
        # `(?=$|\\+)` instead of being anchored only to `$`.
        script = Path("scripts/windows/build-installer.ps1").read_text(
            encoding="utf-8"
        )
        # Old buggy pattern (anchored only to end-of-string) must be gone.
        self.assertNotIn(r"'-rc\.\d+$'", script)
        # New pattern strips before `+` too.
        self.assertIn(r"'-rc\.\d+(?=$|\+)'", script)
        # And the throw message must advertise the RC form so a future
        # refactor doesn't silently drop RC-aware behavior.
        self.assertIn("1.19.0-rc.1", script)

    def test_standalone_windows_installer_derives_is_prerelease_from_tag(self):
        # The manual `.github/workflows/windows-installer.yml` wrapper rebuilds
        # the installer for a previously-shipped tag. release.yml forwards the
        # release-job `outputs.is_prerelease`, but the standalone wrapper has
        # no upstream job — it must derive the prerelease flag from the tag
        # itself, otherwise an RC rebuild leaks the finals-only winget step.
        # Regression for the second P2 #406 Codex finding.
        standalone = Path(
            ".github/workflows/windows-installer.yml"
        ).read_text(encoding="utf-8")
        self.assertIn(
            "is_prerelease: ${{ contains(github.event.inputs.tag, '-rc.')"
            " && 'true' || 'false' }}",
            standalone,
        )
        # And the reusable workflow it calls must still type the input as a
        # string, so the and-or string-conversion above is the correct shape.
        reusable = Path(
            ".github/workflows/windows-installer-build.yml"
        ).read_text(encoding="utf-8")
        # Look for the input block: `is_prerelease:` followed by `type: string`
        # within the same input definition. Allow lines in between for the
        # description block.
        match = re.search(
            r"is_prerelease:\s*\n(?:[ \t]+[^\n]*\n)*?[ \t]+type:\s*string",
            reusable,
        )
        self.assertIsNotNone(
            match,
            "windows-installer-build.yml `is_prerelease` input must be"
            " typed as string so manual-wrapper string forwarding lints.",
        )

    def test_release_tests_call_forwards_paths_filter_permissions(self):
        # The release pipeline gates on `test.yml` via `workflow_call`. The
        # called workflow's `changes` job (paths-filter) needs
        # `pull-requests: read` so reusable-workflow permission resolution
        # can satisfy it without exceeding the caller. When the `tests:`
        # call had no `permissions:` block, the entire release startup-failed
        # on tag push with zero jobs spawned (observed on v1.19.0-rc.1).
        # Lock both scopes into the forwarded permissions so this can't
        # silently regress and brick the next release cut.
        #
        # String-level assertion (no PyYAML dep — CI's unit job ships only
        # stdlib + pytest): grab the `tests:` job block (until the next
        # top-level `release:` job) and assert the permissions are spelled.
        release = Path(".github/workflows/release.yml").read_text(encoding="utf-8")
        match = re.search(
            r"\n  tests:\n(?P<body>.*?)\n  release:\n",
            release,
            re.DOTALL,
        )
        self.assertIsNotNone(
            match, "release.yml must declare a `tests:` job followed by `release:`",
        )
        tests_block = match.group("body")
        self.assertIn("uses: ./.github/workflows/test.yml", tests_block)
        # Permissions block on the call — both scopes spelled out.
        self.assertRegex(
            tests_block,
            r"permissions:\s*\n\s+contents:\s*read\s*\n\s+pull-requests:\s*read",
            "release.yml `tests` call must forward `contents: read` and"
            " `pull-requests: read` so the reusable `changes` job"
            " (dorny/paths-filter) is permitted under the caller's permission"
            " cap — otherwise tag-push releases startup_failure.",
        )

    def test_test_yml_changes_job_is_pr_scoped_with_default_true(self):
        # Paths-filter only makes sense for pull_request events (it needs the
        # PR-files API for an accurate diff). On push to main / workflow_call
        # from a tag release, the filter step is skipped and the `code` output
        # defaults to `true` so every downstream gate runs. Without this
        # fallback the release pipeline either startup_failures or the
        # `Fail required check` step fires on the tag push.
        #
        # String-level assertion (no PyYAML dep): isolate the `changes:` job
        # block and assert (1) the `code` output uses the `|| 'true'`
        # fallback and (2) both the checkout and paths-filter steps are
        # gated on `pull_request`.
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")
        match = re.search(
            r"\n  changes:\n(?P<body>.*?)\n  unit:\n",
            workflow,
            re.DOTALL,
        )
        self.assertIsNotNone(
            match, "test.yml must declare a `changes:` job followed by `unit:`",
        )
        changes_block = match.group("body")
        # Default-true fallback so non-PR triggers (push/workflow_call) skip
        # the filter step yet still evaluate as "real changes present".
        self.assertIn(
            "code: ${{ steps.filter.outputs.code || 'true' }}",
            changes_block,
            "`changes.outputs.code` must default to 'true' when the filter"
            " step is skipped on non-PR events.",
        )
        # PR-scoped: paths-filter step.
        self.assertRegex(
            changes_block,
            r"uses:\s*dorny/paths-filter@[^\n]+\n\s+id:\s*filter\n(?:[ \t]+#[^\n]*\n)*\s+if:\s*github\.event_name\s*==\s*'pull_request'",
            "dorny/paths-filter must only run on pull_request events.",
        )
        # PR-scoped: checkout step that feeds paths-filter.
        self.assertRegex(
            changes_block,
            r"uses:\s*actions/checkout@[^\n]+\n\s+if:\s*github\.event_name\s*==\s*'pull_request'",
            "The checkout that feeds paths-filter must also be PR-scoped so"
            " the `changes` job is a cheap no-op on push/workflow_call.",
        )

    def test_release_builds_ship_all_four_rust_backends(self):
        # rc.2 of Wave 8 (#348): the release pipeline must build with
        # rust-injection, rust-hotkeys, audio-in-rust, AND whisper-rs-local
        # enabled so all four Rust backends actually ship. Earlier RCs only
        # included rust-injection+rust-hotkeys; flipping the remaining two
        # makes the in-process VAD pipeline and local-Whisper inference
        # available out of the box. Lock the exact flag string in all three
        # build invocations (Linux release.yml, Windows reusable installer,
        # and the local-loop scripts/windows/build-installer.ps1) so a
        # future edit can\'t silently drop one of them.
        expected = (
            "--features rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local"
        )
        for rel in (
            ".github/workflows/release.yml",
            ".github/workflows/windows-installer-build.yml",
            "scripts/windows/build-installer.ps1",
        ):
            text = Path(rel).read_text(encoding="utf-8")
            self.assertIn(
                expected, text,
                f"{rel} must build with all four shipping Rust backends",
            )

    def test_release_pipelines_bundle_onnx_runtime_next_to_binary(self):
        # rc.2 of Wave 8 (#348): the `audio-in-rust` feature pulls in
        # vad-rs -> ort, which dynamically loads onnxruntime.dll (Windows)
        # / libonnxruntime.so* (Linux) at startup. ort\'s `copy-dylibs`
        # build feature drops the dylib in target/release/ next to the
        # executable; we then have to ship it next to the installed binary
        # in EVERY shipping artifact: Inno installer, Windows portable
        # ZIP, and the Linux release tarball. Without these bundling
        # steps the app bricks at launch for every fresh user (OS loader
        # fails before `main()` runs) — the regression we explicitly
        # closed #404 to plan against.
        inno = Path("packaging/windows/inno/whisper-dictate.iss").read_text(
            encoding="utf-8",
        )
        self.assertRegex(
            inno,
            r'Source:\s*"\.\.\\\.\.\\\.\.\\target\\release\\onnxruntime\*\.dll";\s*DestDir:\s*"\{app\}";',
            "Inno installer must ship onnxruntime*.dll next to whisper-dictate.exe",
        )
        # skipifsourcedoesntexist so a dev build without the audio-in-rust
        # feature still compiles the .iss locally.
        # Search the Source line for the flag.
        self.assertIn(
            "onnxruntime*.dll",
            inno,
        )
        onnx_line = next(
            line for line in inno.splitlines()
            if "onnxruntime*.dll" in line and "Source:" in line
        )
        self.assertIn(
            "skipifsourcedoesntexist", onnx_line,
            "Inno onnxruntime Source line needs skipifsourcedoesntexist so"
            " local dev builds without audio-in-rust still pack",
        )

        win_pack = Path(
            ".github/workflows/windows-installer-build.yml"
        ).read_text(encoding="utf-8")
        self.assertIn(
            r"Copy-Item target\release\onnxruntime*.dll $bundle",
            win_pack,
            "Windows ZIP must glob onnxruntime*.dll into the bundle",
        )

        linux_pack = Path(".github/workflows/release.yml").read_text(
            encoding="utf-8",
        )
        self.assertIn(
            "libonnxruntime.so",
            linux_pack,
            "Linux tarball must bundle libonnxruntime.so* next to the binary",
        )

    # --- Vulkan short-CARGO_TARGET_DIR regression tests (PR #670) -------
    #
    # Background for all the `test_vulkan_*` methods below: rc.14 attempt 1
    # (job 90252176432) failed after PR #662 fixed the first-level
    # MSBuild-in-MSBuild wedge. whisper.cpp's ExternalProject_Add for
    # vulkan-shaders-gen creates a CMake compiler probe at
    #   <target>/release/build/whisper-rs-sys-<hash>/out/build/
    #     ggml/src/ggml-vulkan/vulkan-shaders-gen-prefix/src/
    #     vulkan-shaders-gen-build/CMakeFiles/CMakeScratch/
    #     TryCompile-<id>/CMakeFiles/cmTC_<id>.dir/testCCompiler.c.obj
    # which reaches ~258 chars from the runner's default
    # D:\a\whisper-dictate\whisper-dictate\ workspace root and trips
    # cl.exe's classic MAX_PATH (empty /Fo filename, "fatal error
    # C1083: Cannot open compiler generated file: '': Invalid argument").
    # OS long-path support does not rescue cl.exe here -- it writes /Fo
    # through plain CreateFile with no `\\?\` prefix -- so the fix is to
    # point cargo's target root at `D:\t` for the Vulkan build and copy
    # the release artefacts back to target\release\ so downstream steps
    # keep reading from the conventional location.
    #
    # The tests were split off the original oversized single method per
    # Codex P2 (AGENTS.md "no oversized methods" rule): each method below
    # focuses on ONE assertion cluster so a failure isolates the exact
    # part of the workaround that regressed.

    def _windows_installer_workflow_text(self):
        return Path(
            ".github/workflows/windows-installer-build.yml"
        ).read_text(encoding="utf-8")

    def _local_installer_script_text(self):
        return Path("scripts/windows/build-installer.ps1").read_text(
            encoding="utf-8"
        )

    def _isolate_windows_build_step_branches(self, workflow):
        """Return (vulkan_branch, cpu_branch) inside the "Build Rust desktop
        UI" step so per-branch assertions cannot be satisfied by the wrong
        branch's content. Fails the caller test if the step is missing or
        no longer has the Vulkan-if / CPU-else split.
        """
        match = re.search(
            r"- name: Build Rust desktop UI\b.*?(?=\n\s{6}- name: )",
            workflow,
            re.DOTALL,
        )
        self.assertIsNotNone(
            match,
            "Could not isolate the 'Build Rust desktop UI' step -- has "
            "its name or the following step boundary changed?",
        )
        build_step = match.group(0)
        self.assertIn(
            "if ($env:VOICEPI_BUILD_VULKAN -ne '0') {",
            build_step,
            "Build step must still branch on VOICEPI_BUILD_VULKAN so "
            "the killswitch works.",
        )
        vulkan_branch, sep, cpu_branch = build_step.partition("} else {")
        self.assertTrue(
            sep,
            "Build step must retain its `} else {` split so the CPU-only "
            "path is reachable.",
        )
        return vulkan_branch, cpu_branch

    def test_vulkan_build_sets_short_cargo_target_dir_to_d_drive(self):
        # The Vulkan branch of the CI build step MUST redirect cargo at
        # D:\t so the nested vulkan-shaders-gen TryCompile probe stays
        # below Windows' 260-char MAX_PATH cap. `D:\`, not `C:\`, because
        # GH-hosted windows-2025 runners reserve the larger volume for
        # D:\a\... and the small C: OS volume can run out of space under
        # a full Rust+whisper.cpp+ONNX release build (Claude review on
        # #670). Regression: removing either the $shortTargetDir literal
        # or the CARGO_TARGET_DIR export re-opens the C1083 failure mode.
        workflow = self._windows_installer_workflow_text()
        vulkan_branch, _ = self._isolate_windows_build_step_branches(workflow)
        self.assertIn(
            "$shortTargetDir = 'D:\\t'",
            vulkan_branch,
            "Vulkan build must define $shortTargetDir = 'D:\\t' (3 chars) "
            "so nested cmake paths stay under the 260 MAX_PATH ceiling.",
        )
        self.assertIn(
            "$env:CARGO_TARGET_DIR = $shortTargetDir",
            vulkan_branch,
            "Vulkan build must export CARGO_TARGET_DIR so cargo AND the "
            "nested cmake ExternalProject write into the short path.",
        )
        self.assertIn(
            "cargo build --manifest-path src/rust/Cargo.toml --target-dir "
            "$shortTargetDir --release -p whisper-dictate-app --features "
            "rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local,"
            "whisper-rs-vulkan",
            vulkan_branch,
            "cargo must be invoked with the short target dir so "
            "whisper-rs-sys writes its ExternalProject scratch there.",
        )

    def test_vulkan_build_copies_release_artefacts_back_to_target_release(self):
        # The copy-back is what lets downstream Verify / Build ZIP / Inno
        # steps -- and the locked
        # test_release_pipelines_bundle_onnx_runtime_next_to_binary
        # `Copy-Item target\release\onnxruntime*.dll $bundle` assertion --
        # keep working without a per-branch fork. Regression: dropping any
        # of the three copies means the Inno installer or portable ZIP
        # ships a broken (or empty) release, not a Vulkan-linked one.
        workflow = self._windows_installer_workflow_text()
        vulkan_branch, _ = self._isolate_windows_build_step_branches(workflow)
        self.assertRegex(
            vulkan_branch,
            r"Copy-Item \(Join-Path \$shortTargetDir 'release\\whisper-dictate\.exe'\)"
            r"\s+target\\release\\ -Force",
            "Vulkan build must copy whisper-dictate.exe back to "
            "target\\release\\ so downstream steps find it.",
        )
        self.assertRegex(
            vulkan_branch,
            r"Copy-Item \(Join-Path \$shortTargetDir 'release\\whisper-dictate-gui\.exe'\)"
            r"\s+target\\release\\ -Force",
            "Vulkan build must copy whisper-dictate-gui.exe back to "
            "target\\release\\ so the Inno installer + portable ZIP ship "
            "the tray launcher.",
        )
        self.assertIn(
            "onnxruntime*.dll",
            vulkan_branch,
            "Vulkan copy-back must include onnxruntime*.dll or the "
            "Windows ZIP + Inno steps ship a runtime-broken artefact.",
        )

    def test_cpu_only_build_does_not_touch_cargo_target_dir(self):
        # The workaround is Vulkan-specific (only the vulkan-shaders-gen
        # ExternalProject_Add creates the deep TryCompile path). Adding a
        # CARGO_TARGET_DIR export to the CPU-only branch would leak the
        # short path into a code path that does not need it and does not
        # have a matching actions/cache entry, silently dropping the
        # Swatinem `src/rust -> ../target` cache for the CPU RC path
        # (Codex P2 #670). The CPU-only cargo invocation must therefore
        # keep its default `--target-dir target`.
        workflow = self._windows_installer_workflow_text()
        _, cpu_branch = self._isolate_windows_build_step_branches(workflow)
        self.assertNotIn(
            "CARGO_TARGET_DIR",
            cpu_branch,
            "CPU-only branch must NOT set CARGO_TARGET_DIR -- the "
            "workaround is Vulkan-specific and CPU builds have no matching "
            "cache entry for a short path (Codex P2 #670).",
        )
        self.assertIn(
            "cargo build --manifest-path src/rust/Cargo.toml --target-dir "
            "target --release -p whisper-dictate-app --features "
            "rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local",
            cpu_branch,
            "CPU-only fallback must keep its default --target-dir target "
            "so Swatinem/rust-cache stays effective.",
        )

    def test_vulkan_build_caches_short_target_directory(self):
        # Without an actions/cache entry keyed on D:\t, every Vulkan RC
        # re-downloads and re-compiles the whole Rust + whisper.cpp +
        # Vulkan tree (~5+ min cold) because Swatinem's workspace-relative
        # cache never sees D:\t (Codex P2 #670). This test locks the
        # dedicated cache step's shape: the Vulkan-only if-guard, the
        # exact four cargo-target subdirs cargo actually reuses, and the
        # -vulkan- key suffix so a Vulkan build can never cross-populate
        # from a CPU-only warm cache (which would silently omit
        # GGML_VULKAN symbols and re-open the #645 regression).
        #
        # The `actions/cache` version pin uses `@v\d+` (was `@v4`) so a
        # future Renovate bump — like #671, which moved this to `@v6`
        # without touching the test — doesn't false-fail. What matters
        # for the wedge signal is that SOME `actions/cache` step exists
        # with the specific if-guard + name, not the exact major
        # version, and the surrounding tests below still pin the cache
        # KEY prefix and subdirs so a real regression bites.
        workflow = self._windows_installer_workflow_text()
        # actions/cache was bumped from v4 to v6 by #671 (Renovate); the
        # regex checks a `v\d+` range so a future minor/major bump does
        # not re-break this pin. The load-bearing invariant is that the
        # dedicated cache step exists with the correct if-guard and IS
        # an actions/cache invocation — not the exact version pin.
        self.assertRegex(
            workflow,
            r"- name: Cache short Vulkan target dir\s*\n"
            r"\s*if: steps\.win-check\.outputs\.changed == 'true' && "
            r"env\.VOICEPI_BUILD_VULKAN != '0'\s*\n"
            r"\s*uses: actions/cache@v\d+",
            "A dedicated actions/cache step must cache the short "
            "Vulkan target dir; without it, every Vulkan RC rebuilds "
            "the whole whisper.cpp + Vulkan tree from scratch.",
        )
        for subdir, why in (
            (
                "D:\\t\\release\\.fingerprint",
                "cargo's .fingerprint dir so incremental rebuilds skip "
                "unchanged units",
            ),
            (
                "D:\\t\\release\\build",
                "the release build dir so whisper-rs-sys's compiled C++ "
                "(the ~5 min bottleneck) is restored",
            ),
            (
                "D:\\t\\release\\deps",
                "deps/ so cargo does not re-link every dependency crate",
            ),
        ):
            self.assertIn(
                subdir,
                workflow,
                f"The Vulkan cache must include {why}.",
            )
        self.assertIn(
            "rust-release-windows-vulkan-shorttarget-v1-",
            workflow,
            "Vulkan cache key must carry the -vulkan- suffix so it never "
            "cross-populates from a CPU-only warm cache.",
        )

    def test_local_installer_uses_short_cargo_target_dir_for_vulkan(self):
        # Same short-path fix mirrored in the local installer script so a
        # developer with a deep project path (e.g. D:\source\projects\
        # voicepi\whisper-dictate\) doesn't hit the same C1083 wall in
        # the local loop. Local default is C:\t (not D:\t) because D:\ is
        # not guaranteed on dev boxes; a developer-set CARGO_TARGET_DIR
        # is still honoured.
        script = self._local_installer_script_text()
        self.assertIn(
            "$env:CARGO_TARGET_DIR = $shortTargetDir",
            script,
            "Local Vulkan build must also override CARGO_TARGET_DIR so "
            "the local loop matches CI's behaviour.",
        )
        self.assertIn(
            "'C:\\t'",
            script,
            "Local Vulkan build must default to the same short target "
            "dir (`C:\\t` locally since D:\\ is not guaranteed on dev "
            "boxes) unless the developer already set CARGO_TARGET_DIR.",
        )

    def test_local_installer_restores_cargo_target_dir_after_vulkan_build(self):
        # The local script mutates process-scope CARGO_TARGET_DIR; without
        # a try/finally restore the developer's shell inherits `C:\t` for
        # every subsequent cargo command, including commands in other
        # repositories (Codex P2 #670). Assert the exact snapshot line,
        # the try/finally wrapping the assignment + build + copy-back, and
        # the `Remove-Item env:CARGO_TARGET_DIR` on the unset-restore
        # branch. `Test-Path env:...` (not truthiness) is what
        # distinguishes "was unset" from "was empty string" so the
        # restore never re-introduces a bogus empty value for a variable
        # the developer never touched.
        script = self._local_installer_script_text()
        self.assertRegex(
            script,
            r"\$prevCargoTargetDirWasSet = Test-Path env:CARGO_TARGET_DIR",
            "Local script must snapshot whether CARGO_TARGET_DIR was set "
            "before the build so the restore distinguishes unset vs "
            "empty string.",
        )
        self.assertRegex(
            script,
            r"try \{\s*\n\s*\$env:CARGO_TARGET_DIR = \$shortTargetDir",
            "The CARGO_TARGET_DIR assignment must be inside a try block "
            "so a build failure still hits the restore in `finally`.",
        )
        self.assertRegex(
            script,
            r"\} finally \{\s*\n"
            r"\s*if \(\$prevCargoTargetDirWasSet\) \{\s*\n"
            r"\s*\$env:CARGO_TARGET_DIR = \$prevCargoTargetDir\s*\n"
            r"\s*\} else \{\s*\n"
            r"\s*Remove-Item env:CARGO_TARGET_DIR",
            "Local script must restore CARGO_TARGET_DIR to its exact "
            "pre-build state (unset -> Remove-Item; set -> reassign) so "
            "the developer's shell environment is not permanently "
            "polluted by `C:\\t`.",
        )

    def test_test_yml_builds_whisper_rs_local_on_both_runners(self):
        # rc.2 of Wave 8 (#348): adding `whisper-rs-local` to the release
        # build means whisper.cpp must compile cleanly on both ubuntu-latest
        # and windows-2025 runners. A build-only smoke step in the `rust`
        # matrix job catches link/build breaks BEFORE the release pipeline
        # hits them — without it the release tag is the first place a
        # whisper-rs API break surfaces, which is the wrong gate.
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")
        self.assertIn(
            "Rust build (whisper-rs-local feature)", workflow,
            "test.yml rust matrix must include a whisper-rs-local smoke step",
        )
        # The release-profile smoke must include BOTH the whisper-rs-local
        # feature (link surface) AND audio-capture (so the release binary
        # carries the CLI verbs the Windows CLI-output smoke asserts on).
        # Order-agnostic: cargo accepts features in either order.
        self.assertRegex(
            workflow,
            r"--features whisper-rs-local,audio-capture --release"
            r"|--features audio-capture,whisper-rs-local --release",
            msg="whisper-rs-local smoke must build the release profile with"
            " audio-capture too so the Windows CLI-output smoke has the audio"
            " verbs available",
        )

    def test_test_workflow_asserts_release_cli_prints_on_windows(self):
        # Regression guard for the `windows_subsystem = "windows"` bug class
        # (PR #564): a release binary compiled with the GUI subsystem attribute
        # printed NOTHING for any CLI verb when run from PowerShell, because
        # no console was attached. Debug builds hide this — the attribute is
        # gated on `not(debug_assertions)` — so the guard MUST be on a
        # release build with a stdout-content assertion. Pin the step so it
        # cannot silently regress or lose its content check.
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")
        self.assertIn("Windows release CLI-output smoke", workflow)
        self.assertIn("runner.os == 'Windows'", workflow)
        self.assertIn("target/release/whisper-dictate.exe", workflow)
        # Both content-asserting verbs must be exercised — a stub that only
        # ran the exe with `--version` would pass even for a GUI-subsystem
        # binary if the release-code path went through a different init.
        self.assertIn("config path", workflow)
        self.assertIn("self-test audio-capture --json", workflow)
        # The failure messages must name the two bug classes this smoke
        # separates: the definitive PE-subsystem check (the class this
        # guard specifically exists for), and the content checks (missing
        # verb dispatch / init panic — orthogonal, but same guard).
        self.assertIn("windows_subsystem = 'windows'", workflow)
        self.assertIn("CLI dispatch is broken", workflow)
        # And the check MUST be on stdout content (IsNullOrWhiteSpace), not
        # on $LASTEXITCODE — self-test audio-capture exits non-zero on a
        # headless CI runner without a mic, and gating on exit code would
        # give a false positive for the class of bug we actually care about.
        self.assertIn("IsNullOrWhiteSpace", workflow)
        self.assertNotIn("$LASTEXITCODE -ne 0 -and", workflow)
        # The DEFINITIVE guard is the PE Optional Header Subsystem check
        # (Codex #565 P1): a `& $exe` capture in PowerShell gives even a
        # GUI-subsystem process an explicit stdout pipe handle, so a
        # content-only check is tautological against the exact binary
        # this smoke exists to reject. Bit-exact PE inspection cannot be
        # gamed by shell redirection.
        self.assertIn("PE Optional Header Subsystem", workflow)
        self.assertIn("IMAGE_SUBSYSTEM_WINDOWS_CUI", workflow)
        # Pin the two offsets so a future refactor cannot silently move
        # them to the wrong bytes and turn the guard into a no-op.
        self.assertIn("0x3C", workflow)  # e_lfanew in DOS header
        self.assertIn("0x5C", workflow)  # Subsystem offset from PE signature
        # The audio JSON envelope check must PARSE the JSON and assert the
        # stable `kind` field (Codex #565 P2) — a bare `IsNullOrWhiteSpace`
        # on the raw stdout passes for truncated JSON or a stray diagnostic
        # line, masking real envelope regressions.
        self.assertIn("ConvertFrom-Json", workflow)
        self.assertIn("audio_capture_self_test", workflow)
        # The step MUST end with an executable `exit 0` — GitHub's pwsh
        # shell propagates $LASTEXITCODE from the last native call, and
        # without this the audio verb's environmental non-zero exit (no mic
        # on the CI runner) shadows the step's success and false-fails the
        # check even after both content assertions pass. Learned the hard
        # way in the first CI run of this exact guard.
        #
        # Strip PowerShell comment lines before asserting so a regression
        # that removes the executable `exit 0` (leaving only its narrative
        # comment) actually trips this test — Codex #565 P2 caught the
        # original `assertIn("exit 0", smoke_step)` shape false-passing
        # against comment prose that also said "exit 0".
        smoke_step = workflow.split("Windows release CLI-output smoke", 1)[1].split(
            "Rust CLI smoke", 1
        )[0]
        smoke_code_lines = [
            line for line in smoke_step.splitlines()
            if not line.strip().startswith("#")
        ]
        smoke_code = "\n".join(smoke_code_lines)
        # An executable `exit 0` line has no preceding `#`; the split above
        # already dropped every `# …` comment line entirely.
        self.assertRegex(
            smoke_code,
            r"(?m)^\s*exit 0\s*$",
            msg="Windows release CLI-output smoke MUST end with an executable"
            " `exit 0` line (not just a comment mentioning it) so the audio"
            " verb's environmental non-zero exit does not shadow the step",
        )

    def test_release_linux_deps_cover_audio_in_rust_alsa_chain(self):
        # The `audio-in-rust` feature pulls in cpal -> alsa-sys, which needs
        # `libasound2-dev` to find `alsa.pc` via pkg-config at build time.
        # rc.2 flipped audio-in-rust into the shipping `--features` list, but
        # the release.yml Linux apt step initially shipped without libasound,
        # which made the release job hard-fail at `cargo build` with
        # "Package \'alsa\', required by \'virtual:world\', not found".
        # test.yml\'s rust matrix already installs it (line ~320); pinning
        # release.yml in lockstep keeps the rc-N -> release pipeline gated.
        release = Path(".github/workflows/release.yml").read_text(encoding="utf-8")
        self.assertIn(
            "libasound2-dev", release,
            "release.yml Linux apt step must install libasound2-dev for the"
            " audio-in-rust cpal/alsa-sys build chain shipped in rc.2.",
        )

    def test_warm_release_cache_runs_on_nightly_schedule_not_per_push(self):
        # Wave 5/8 churn produced 3-5 Rust-touching main merges per day,
        # each triggering a Windows cache warm-up (~5 min) thats almost
        # always thrown away before the next ~weekly release consumes it.
        # The trigger must be a daily schedule + workflow_dispatch only;
        # ANY `push:` block reintroduces the spam. workflow_dispatch is
        # kept so a release captain can pre-warm immediately before tag.
        workflow = Path(".github/workflows/warm-release-cache.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            "schedule:", workflow,
            "warm-release-cache must run on a cron schedule, not per main push",
        )
        self.assertIn(
            "cron:", workflow,
            "schedule block needs a cron expression",
        )
        self.assertIn(
            "workflow_dispatch:", workflow,
            "manual pre-release warm-up entry point must remain",
        )
        # No `push:` trigger -- thats what was spamming CI.
        # Strip comments so the `on:` block check ignores the WHY-this-changed
        # commentary at the top of the file.
        lines_no_comments = [
            ln for ln in workflow.splitlines()
            if not ln.lstrip().startswith("#")
        ]
        body = "\n".join(lines_no_comments)
        self.assertNotIn(
            "push:", body,
            "warm-release-cache must NOT trigger on push -- the schedule + "
            "manual dispatch is enough and the per-push spam was the bug",
        )

    def test_rust_aggregator_gates_on_rust_features_matrix(self):
        # Codex P2 #581: the `rust` job is the required-status contract
        # for branch protection (`rust (ubuntu-latest)` /
        # `rust (windows-2025)`), and the 2026-07-26 refactor made it a
        # thin aggregator over the parallel `rust-features` matrix. Lock:
        # (a) both required matrix legs still exist,
        # (b) the aggregator explicitly needs `rust-features` (else the
        #     required check would go green with a broken matrix),
        # (c) the failure gate is present so a non-success/non-skipped
        #     rust-features result BUBBLES UP — silent-skip would hide
        #     regressions in the parallel legs.
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")
        m = re.search(
            r"\n  rust:\n(?P<body>.*?)\n  rust-release:\n",
            workflow,
            re.DOTALL,
        )
        self.assertIsNotNone(
            m,
            "test.yml must declare `rust:` immediately before `rust-release:`",
        )
        rust_body = m.group("body")
        # (a) both required matrix legs preserved.
        self.assertIn("os: [ubuntu-latest, windows-2025]", rust_body)
        # (b) aggregator depends on BOTH rust-features AND rust-release
        # (Codex P2 #581: without rust-release in `needs`, a red Windows
        # release guard would leave the required context green and let
        # regressions merge). Order-agnostic on `changes`; both feature
        # and release jobs must appear.
        m2 = re.search(r"needs:\s*\[([^\]]+)\]", rust_body)
        self.assertIsNotNone(m2, "`rust:` must declare a `needs:` list")
        needs_list = {n.strip() for n in m2.group(1).split(",")}
        self.assertIn("changes", needs_list)
        self.assertIn("rust-features", needs_list)
        self.assertIn(
            "rust-release", needs_list,
            "the `rust` aggregator must also `needs: rust-release` so a"
            " red Windows release guard fails the required context (Codex P2 #581)",
        )
        # (c) explicit fail step on non-success/non-skipped rust-features result.
        self.assertIn(
            "needs.rust-features.result != 'success' && needs.rust-features.result != 'skipped'",
            rust_body,
            "the aggregator must FAIL when rust-features didnt succeed"
            " (skipped is fine — that is the docs-only-PR path)",
        )
        # (d) same guard for rust-release (Codex P2 #581 follow-up).
        self.assertIn(
            "needs.rust-release.result != 'success' && needs.rust-release.result != 'skipped'",
            rust_body,
            "the aggregator must FAIL when rust-release didnt succeed"
            " so Windows regressions (#564 / Codex #518 F6) block the required check",
        )

    def test_rust_release_keeps_windows_guards_on_pr_ci(self):
        # Claude-bot review PR #581: the Windows-specific regression
        # guards (release CLI-output smoke for PR #564 windows_subsystem
        # bug, and injection-idempotency for Codex #518 F6 SendInput
        # leakage) have no equivalent elsewhere in this workflow —
        # `integration-ubuntu-2604` is Linux-only. Moving them to a
        # push-only job would let a regression merge and only surface
        # post-merge. Lock: the rust-release job condition must NOT
        # exclude pull_request events outright, and its RUN_RELEASE
        # env expression must gate the ubuntu-latest leg (not the
        # windows-2025 leg) on non-PR events.
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")
        m = re.search(
            r"\n  rust-release:\n(?P<body>.*?)(?:\n  [a-zA-Z]|\Z)",
            workflow,
            re.DOTALL,
        )
        self.assertIsNotNone(m, "test.yml must declare a `rust-release:` job")
        rr_body = m.group("body")
        # The job-level `if:` must NOT exclude pull_request wholesale —
        # a regression to `if: !cancelled() && github.event_name != 'pull_request'`
        # would silently drop the Windows guards from PR CI. Only the
        # ubuntu-latest leg may be PR-excluded, and that gets done via
        # the RUN_RELEASE env below.
        self.assertNotRegex(
            rr_body,
            r"if:\s*\$\{\{\s*[^}]*github\.event_name\s*!=\s*'pull_request'[^}]*\}\}",
            "rust-release job-level `if:` must NOT exclude pull_request events;"
            " that removes Windows guards (#564 + Codex #518 F6) from PR CI",
        )
        # The RUN_RELEASE expression must scope the PR-excluded branch to
        # the ubuntu leg (matrix.os == 'windows-2025' escape hatch keeps
        # Windows running on PR).
        self.assertIn("matrix.os == 'windows-2025'", rr_body)
        # Both regression-guard step names must still live in the file
        # (they were moved OUT of the required `rust` aggregator, so a
        # future refactor could accidentally delete them entirely).
        self.assertIn("Windows release CLI-output smoke", workflow)
        self.assertIn("injection-idempotency regression test", workflow)

    def test_changes_filter_splits_per_language_and_gates_jobs(self):
        # 2026-07-26 (#590 follow-up): the changes job emits three
        # outputs — `code` (catch-all), `rust`, and `python` — so a
        # pure-Python PR skips the ~5-15 min Rust matrix and a pure-Rust
        # PR skips the ~1-2 min Python legs. Lock:
        # (a) all three outputs declared with the same `|| 'true'`
        #     fallback so non-PR events (push/workflow_call) still fire
        #     every job.
        # (b) `rust` and `python` filters BOTH include `.github/workflows/**`
        #     so a workflow YAML change still exercises the full matrix
        #     (documented invariant — silent-skipping YAML changes is how
        #     broken pipelines merge).
        # (c) `rust` filter includes shared config the Rust build reads
        #     (schemas Rust deserializes via serde, sonar properties).
        # (d) each downstream job uses the correct output: rust-features
        #     + rust-release gate on `rust`, unit + smoke gate on
        #     `python`, groq-integration + integration-ubuntu-2604 gate
        #     on either.
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        # (a) three outputs declared with the fallback.
        for out in ("code", "rust", "python"):
            self.assertRegex(
                workflow,
                rf"{out}:\s*\$\{{\{{\s*steps\.filter\.outputs\.{out}\s*\|\|\s*'true'\s*\}}\}}",
                f"changes job must declare `{out}` output with `|| 'true'`"
                " fallback so non-PR events run every downstream job",
            )

        # (b) both language filters include workflow YAML.
        filter_block = re.search(
            r"filters:\s*\|(?P<body>.*?)(?:\n  [a-zA-Z]|\Z)",
            workflow,
            re.DOTALL,
        )
        self.assertIsNotNone(filter_block, "filters: block must be present")
        fb = filter_block.group("body")
        for lang in ("rust", "python"):
            block = re.search(
                rf"\n            {lang}:\n(?P<body>(?:              - '[^']+'\n)+)",
                fb,
            )
            self.assertIsNotNone(block, f"`{lang}` filter block must exist")
            self.assertIn(
                "'.github/workflows/**'", block.group("body"),
                f"`{lang}` filter MUST include .github/workflows/** so a"
                " workflow YAML change exercises the full matrix",
            )

        # (c) rust filter must also cover shared config paths.
        rust_block = re.search(
            r"\n            rust:\n(?P<body>(?:              - '[^']+'\n)+)",
            fb,
        )
        self.assertIsNotNone(rust_block)
        for shared in (
            "'src/rust/**'",
            "'rust-toolchain.toml'",
            "'src/python/whisper_dictate/schemas/**'",
        ):
            self.assertIn(
                shared, rust_block.group("body"),
                f"`rust` filter must include {shared} so a change there"
                " triggers the rust matrix",
            )

        # (d) downstream gate wiring: extract each job's RUN_* env value
        # and assert the correct output is referenced.
        cases = [
            ("unit", "RUN_UNIT", "python"),
            ("smoke", "RUN_SMOKE", "python"),
            ("rust-features", "RUN_RUST", "rust"),
        ]
        for job_name, env_var, expected_output in cases:
            m = re.search(
                rf"\n  {re.escape(job_name)}:\n(?P<body>.*?)(?:\n  [a-zA-Z]|\Z)",
                workflow,
                re.DOTALL,
            )
            self.assertIsNotNone(m, f"job `{job_name}` must exist")
            body = m.group("body")
            self.assertIn(
                f"needs.changes.outputs.{expected_output} == 'true'", body,
                f"{job_name}'s {env_var} must gate on"
                f" `needs.changes.outputs.{expected_output}` for the"
                " per-language skip to work",
            )

        # rust-release specifically must gate on `rust`, NOT `code`
        # (a pure-Python PR shouldn't spin up a Windows release compile).
        rr = re.search(
            r"\n  rust-release:\n(?P<body>.*?)(?:\n  [a-zA-Z]|\Z)",
            workflow, re.DOTALL,
        )
        self.assertIsNotNone(rr)
        self.assertIn(
            "needs.changes.outputs.rust == 'true'", rr.group("body"),
            "rust-release must gate on `rust` so a pure-Python PR skips"
            " the Windows release-profile compile",
        )

        # integration-ubuntu-2604 must gate on EITHER language (container
        # exercises both).
        ci = re.search(
            r"\n  integration-ubuntu-2604:\n(?P<body>.*?)(?:\n  [a-zA-Z]|\Z)",
            workflow, re.DOTALL,
        )
        self.assertIsNotNone(ci)
        self.assertIn(
            "needs.changes.outputs.rust == 'true'", ci.group("body"),
        )
        self.assertIn(
            "needs.changes.outputs.python == 'true'", ci.group("body"),
        )

    def test_claude_review_never_gets_paths_ignore_filter(self):
        # Codex P1 #587 (confirmed by user Larswa in the same review):
        # `claude-review.yml` subscribes to `pull_request: types: [opened]`
        # ONLY, so it fires exactly once per PR. A `paths-ignore` on that
        # sole trigger means a PR opened with just a prose file gets no
        # review, and later code pushes fire `synchronize` events with
        # NO subscriber — so the PR silently reaches merge unreviewed.
        # Sonar can afford the filter because it re-runs on every push;
        # this workflow cannot. Lock: paths-ignore must NEVER appear
        # under claude-review.yml's pull_request block.
        workflow = Path(".github/workflows/claude-review.yml").read_text(encoding="utf-8")
        # The workflow subscribes to `opened`-only — that's the invariant
        # that makes paths-ignore dangerous. If a future refactor moves
        # to `synchronize` too, revisit whether paths-ignore is safe.
        self.assertIn(
            "types: [opened]", workflow,
            "claude-review is `opened`-only by design; the no-paths-ignore"
            " rule below depends on this invariant",
        )
        self.assertNotIn(
            "paths-ignore:", workflow,
            "claude-review must NOT have paths-ignore — see Codex P1 #587."
            " Filtering an `opened`-only trigger means a prose-first PR"
            " gets zero automatic reviews for its whole lifetime",
        )

    def test_sonar_keeps_paths_ignore_for_prose_only_prs(self):
        # Complement to test_claude_review_never_gets_paths_ignore_filter:
        # sonar re-runs on every push (`synchronize` implicit), so
        # skipping a prose-only opened event is safe — the first code
        # push recovers analysis. The ~4 min per prose PR IS worth
        # skipping. Lock: sonar's paths-ignore stays in place.
        workflow = Path(".github/workflows/sonar.yml").read_text(encoding="utf-8")
        self.assertIn(
            "paths-ignore:", workflow,
            "sonar.yml should keep paths-ignore for prose-only PRs —"
            " it re-runs on every subsequent push so skipping is safe,"
            " and the ~4 min saving is worth banking",
        )
        # The exclusion list must cover the same top-level prose set
        # test.yml's `changes` filter uses, so the two skip in lockstep.
        for pattern in ("'*.md'", "'LICENSE'", "'.gitignore'"):
            self.assertIn(
                pattern, workflow,
                f"sonar.yml paths-ignore must include {pattern} to match"
                " test.yml's changes filter",
            )

    def test_rust_ci_uses_apt_pkgs_cache_on_linux_legs(self):
        # 2026-07-26 follow-up PR to #581 — the ONE clean wall-clock
        # win that survived Codex P2 review: cache the 12 apt packages
        # both rust-features and rust-release install on Linux (~30-40s
        # → ~5s on warm runs). The three other candidates (shared
        # Swatinem cache-key, CARGO_INCREMENTAL env, merged
        # features-combined cell) were reverted after Codex flagged
        # them: shared-key regresses to whoever won the immutable-key
        # save race, incremental state is excluded from Swatinem's
        # save so it costs disk with zero cross-invocation reuse, and
        # a combined-feature cell hides the "feature X alone doesn't
        # compile" bug class since cargo features are additive.
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        # apt-cache action on both Linux legs; raw `sudo apt install`
        # in either job means someone reverted the cache.
        self.assertIn(
            "awalsh128/cache-apt-pkgs-action", workflow,
            "Linux legs must use cache-apt-pkgs-action so the 12-package"
            " install is a warm-cache read on subsequent runs",
        )
        rf = re.search(
            r"\n  rust-features:\n(?P<body>.*?)\n  rust:\n",
            workflow, re.DOTALL,
        )
        self.assertIsNotNone(rf, "test.yml must declare rust-features: before rust:")
        rf_body = rf.group("body")
        self.assertNotIn(
            "sudo apt install", rf_body,
            "raw apt install in rust-features defeats the cache",
        )
        rr = re.search(
            r"\n  rust-release:\n(?P<body>.*?)(?:\n  [a-zA-Z]|\Z)",
            workflow, re.DOTALL,
        )
        self.assertIsNotNone(rr, "test.yml must declare rust-release:")
        self.assertNotIn(
            "sudo apt install", rr.group("body"),
            "raw apt install in rust-release defeats the cache",
        )
        # Standalone hotkeys + audio cells must stay: cargo features
        # are additive, so a combined-only build hides feature-only
        # compile errors (Codex P2 #585 line 315).
        self.assertIn(
            "id: hotkeys\n", rf_body,
            "standalone hotkeys cell must exist — a combined-features"
            " test misses `--features rust-hotkeys` alone regressions",
        )
        self.assertIn(
            "id: audio\n", rf_body,
            "standalone audio cell must exist — a combined-features"
            " test misses `--features audio-in-rust` alone regressions",
        )
        # Per-profile cache key is correct here (Codex P2 #585 line 372):
        # a shared-key across parallel cells regresses to whoever wins
        # the immutable save race. Guard that shared-key: features is
        # NOT reintroduced.
        self.assertNotIn(
            "shared-key: features", rf_body,
            "rust-features must use per-profile `key:`, not shared-key —"
            " actions/cache is immutable so only the first cell to save"
            " wins and every subsequent restore gets its cache, not each"
            " cell's own",
        )
        # Incremental env vars must stay OFF (Codex P2 #585 line 331):
        # Swatinem excludes the incremental/ dir from the saved cache,
        # so the env-vars just cost disk without cross-invocation reuse.
        self.assertNotIn(
            "CARGO_INCREMENTAL", rf_body,
            "CARGO_INCREMENTAL adds disk/bookkeeping with no cross-run"
            " reuse (Swatinem excludes incremental/) — leave it off",
        )

    def test_dev_check_wrapper_mirrors_ci_rust_matrix_legs(self):
        # Codex P2 #418 dev-check.ps1:121 + :51: the pre-push wrapper
        # drives per-feature test invocations so a feature-gated regression
        # is caught locally BEFORE the push. This is DELIBERATELY stricter
        # than CI's `features-combined` cell — running hotkeys and audio
        # separately locally catches "feature X alone doesn't compile"
        # regressions that the combined-features CI cell can't see (cargo
        # features are additive: passing A+B always includes A alone, so
        # the reverse compile-check is only meaningful when done alone).
        script = Path("scripts/dev/dev-check.ps1").read_text(encoding="utf-8")
        for needle in [
            "cargo fmt --all -- --check",
            "cargo clippy (ui-egui-glow)",
            "cargo test (default)",
            "cargo test --features rust-hotkeys",
            "cargo test --features rust-hotkeys,rust-injection",
            "cargo test --features audio-in-rust",
            "cargo build --features whisper-rs-local --release",
            "cargo run -- --help",
            "cargo run -- config path",
        ]:
            self.assertIn(
                needle, script,
                f"dev-check.ps1 must drive `{needle}` for local pre-push coverage",
            )
        # Also assert the wrapper still uses Docker Desktop, not the
        # earlier rancher-desktop WSL routing (which kept failing on
        # silent distro unregistration).
        self.assertIn("--context $DockerContext", script)
        self.assertIn("desktop-linux", script)
        self.assertNotIn("rancher-desktop", script)

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
        self.assertIn("cargo clippy --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app --all-targets --features ui-egui-glow --message-format=json > clippy-report.json", workflow)
        self.assertRegex(workflow, r"uses:\s*SonarSource/sonarqube-scan-action@v\d+")
        self.assertIn("SONAR_TOKEN: ${{ secrets.SONAR_TOKEN }}", workflow)
        self.assertIn("components: clippy", test_workflow)
        self.assertIn("cargo clippy --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app --all-targets --features ui-egui-glow -- -D warnings", test_workflow)

    def test_sonar_imports_python_and_rust_coverage(self):
        sonar = Path("sonar-project.properties").read_text(encoding="utf-8")
        workflow = Path(".github/workflows/sonar.yml").read_text(encoding="utf-8")
        test_workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        # Coverage report paths are wired into the Sonar properties.
        self.assertIn("sonar.python.coverage.reportPaths=coverage.xml", sonar)
        self.assertIn("sonar.rust.lcov.reportPaths=lcov.info", sonar)

        # sonar.yml must generate both reports before the scan runs.
        self.assertIn("components: clippy, llvm-tools-preview", workflow)
        self.assertIn("tool: cargo-llvm-cov", workflow)
        self.assertIn(
            'cargo llvm-cov --manifest-path src/rust/Cargo.toml -p whisper-dictate-app --features ui-egui-glow,whisper-rs-local,audio-in-rust,rust-injection --lcov --output-path "$GITHUB_WORKSPACE/lcov.info"',
            workflow,
        )
        self.assertIn(
            'feature_arg: "--features whisper-rs-local,audio-in-rust,rust-injection"',
            test_workflow,
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
        # Guard the Node24 floor: every actions/setup-python must be v6+ (reject
        # the deprecated Node20 v5 and older). Regex-based rather than pinned to
        # an exact major so Renovate can bump v6 -> v7 -> ... without tripping
        # this guard. Mirrors test_workflows_use_node24_checkout_action. Scan
        # both extensions so a future `.yaml` workflow can't slip past.
        workflows = [
            *Path(".github/workflows").glob("*.yml"),
            *Path(".github/workflows").glob("*.yaml"),
        ]
        majors = []
        for path in workflows:
            workflow = path.read_text(encoding="utf-8")
            for major in re.findall(r"uses:\s*actions/setup-python@v(\d+)", workflow):
                majors.append((path.as_posix(), int(major)))
        self.assertTrue(majors, "no workflow uses actions/setup-python")
        for path, major in majors:
            self.assertGreaterEqual(
                major, 6, f"{path} uses setup-python older than v6 (Node20)"
            )

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

    def test_renovate_config_present_and_dependabot_removed(self):
        # Dependency automation migrated from Dependabot to Renovate (org
        # standard). Renovate's config:recommended auto-detects every ecosystem
        # (pip, cargo, github-actions, docker, ...) with no per-ecosystem or
        # directory blocks, so asserting the recommended preset is how we keep
        # pip + cargo + actions covered after the migration.
        config = json.loads(Path("renovate.json").read_text(encoding="utf-8"))
        self.assertIn(
            "local>FactusConsulting/renovate-config", config.get("extends", [])
        )
        self.assertFalse(
            Path(".github/dependabot.yml").exists(),
            "dependabot.yml must be removed after migrating to Renovate",
        )

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

    def test_spellcheck_is_pr_only_and_limited_to_docs(self):
        # spellcheck.yml was merged into docs.yml alongside markdown-lint.
        # The cspell JOB stays PR-only via a job-level
        # `if: github.event_name == 'pull_request'`, because the sibling
        # markdownlint job DOES run on main pushes.
        workflow = Path(".github/workflows/docs.yml").read_text(encoding="utf-8")
        config = Path("cspell.json").read_text(encoding="utf-8")

        self.assertIn("pull_request:", workflow)
        self.assertNotIn("workflow_call:", workflow)
        self.assertIn("if: github.event_name == 'pull_request'", workflow)
        self.assertIn("npx --yes cspell@", workflow)
        self.assertIn("--config cspell.json", workflow)
        self.assertIn('README.md AGENTS.md CONTRIBUTING.md "docs/**/*.md"', workflow)
        self.assertIn("permissions:\n  contents: read", workflow)
        self.assertIn(
            "cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
            workflow,
        )
        self.assertIn('"language": "en"', config)
        self.assertIn('"dictionaries": ["en_US"]', config)

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

    def test_configuration_reference_is_schema_generated_with_markers(self):
        # The hand-maintained "Cheat sheet" / "Environment variables" tables were
        # replaced by a single block generated from settings_schema.json (the
        # single source of truth) by scripts/dev/gen_settings_docs.py. Guard that
        # the generated block lives between its markers and uses the documented
        # column layout; the drift content itself is checked by
        # src/tests/python/test_settings_docs_generated.py.
        doc = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")
        begin = "<!-- BEGIN GENERATED SETTINGS REFERENCE -->"
        end = "<!-- END GENERATED SETTINGS REFERENCE -->"
        self.assertIn(begin, doc)
        self.assertIn(end, doc)
        self.assertLess(doc.index(begin), doc.index(end))
        self.assertIn("| Key | Env var | Default | Live/Restart | Description |", doc)

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
        # nix/package.nix is bumped in the pre-release version PR (with VERSION).
        # winget manifests are generated and shipped as a release asset (not
        # committed). The Homebrew tap push targets a different repo (cd tap) and
        # is unaffected.
        for path in (
            Path(".github/workflows/release.yml"),
            Path(".github/workflows/windows-installer.yml"),
        ):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("Bump nix/package.nix version", workflow, path.as_posix())
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
            "-p whisper-dictate-app --all-targets --features ui-egui-glow -- -D warnings",
            workflow,
        )
        self.assertIn(
            "cargo test --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app",
            workflow,
        )
        self.assertIn("python -m pytest src/python/tests src/tests/python", workflow)
