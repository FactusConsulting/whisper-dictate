# Release script ownership

Workflow YAML retains triggers, path/matrix gates, caches, toolchain setup,
artifact staging and the pre-publication dependency graph. PowerShell helpers
own executable procedures:

- `scripts/windows/build-release-binaries.ps1`: reviewed feature resolver,
  named/legacy CPU or Vulkan argv, Ninja/short target and portable CPU guards.
- `scripts/windows/tests/smoke-release-binaries.ps1`: PE subsystem, headless
  CLI and audio self-test JSON contract.
- `smoke-installed-layout.ps1`: staged silent installation and version/payload.
- `smoke-controller.ps1`: CLI exit codes and native doctor JSON.
- `smoke-gui-launch.ps1`: pinned software renderer verified by checksum and tray survival.

The Windows builder materializes its binary helper from `BUILD_RECIPE_SHA`,
alongside the feature resolver and packaging helpers. Install smoke checks out
`github.workflow_sha` for reviewed validation tooling, not an arbitrary latest
branch or a historical application tag that predates these helpers.

The scripts preserve existing process argv and environment contracts. CPU and
legacy fallback execution is covered by a hardware-free Windows fixture;
Windows shipping CI verifies the real Vulkan build/link path. Real microphone
and foreground dictation remain manual verification on physical hardware.
