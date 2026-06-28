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
        self.assertIn(
            "--features whisper-rs-local --release", workflow,
            "whisper-rs-local smoke must build the release profile so it"
            " exercises the same code path the shipping installer takes",
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

        # Coverage report paths are wired into the Sonar properties.
        self.assertIn("sonar.python.coverage.reportPaths=coverage.xml", sonar)
        self.assertIn("sonar.rust.lcov.reportPaths=lcov.info", sonar)

        # sonar.yml must generate both reports before the scan runs.
        self.assertIn("components: clippy, llvm-tools-preview", workflow)
        self.assertIn("tool: cargo-llvm-cov", workflow)
        self.assertIn(
            'cargo llvm-cov --manifest-path src/rust/Cargo.toml -p whisper-dictate-app --features ui-egui-glow --lcov --output-path "$GITHUB_WORKSPACE/lcov.info"',
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
