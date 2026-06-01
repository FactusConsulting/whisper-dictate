# Repository Instructions

## Regression Tests

Whenever fixing a bug, add a regression test in the same change unless there is
a clear technical reason not to. Prefer the narrowest useful test:

- Unit tests for pure logic, parsing, configuration, command construction, and
  small platform-specific guards.
- Integration or smoke tests when the bug is in process launch, installer
  behavior, runtime wiring, dependency setup, or cross-module behavior.
- Both when the bug has a small isolated cause and a higher-level workflow that
  could regress independently.

If a regression test is not practical, document the reason in the commit or PR
summary and include the manual verification that covers the bug.

## Project-Specific Expectations

- Treat Windows as the primary supported desktop path. Changes to the Rust
  launcher/controller, installer, subprocess handling, console encoding,
  Settings UI behavior, and keyboard/text injection must be reviewed for
  Windows behavior, not just platform-neutral Python logic.
- Use the local installer loop for internal Windows testing. When changing
  installer files, shortcuts, bundled files, Rust UI/controller behavior, or
  Windows launch behavior, build a local installer with
  `scripts/build-windows-installer.ps1` and report the generated
  `Output\*.exe` and `Output\*.zip`.
- Do not create GitHub releases as part of normal iteration. Build local
  installers by default; create a release only when explicitly requested.
- Keep dictionary and prompt changes bounded. Any change to dictionary loading,
  prompt construction, term selection, or replacements must preserve prompt
  length caps and include tests for both `terms` and `replacements` behavior.
- Preserve the Windows unified controller model. The Rust UI owns the managed
  runtime process on Windows, must avoid duplicate UI instances, and must make
  start, stop, restart, and required restarts explicit.
- Keep terminal and subprocess output Windows-safe. New console output should
  be ASCII-safe or UTF-8-safe with a tested fallback, especially for PowerShell,
  cmd.exe, hidden launchers, and Qt subprocess logs.
- Keep Whisper and Parakeet configuration separate. Parakeet must use its own
  model defaults, dependency checks, and CUDA readiness checks rather than
  inheriting Whisper model names such as `large-v3`.
