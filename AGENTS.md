# Repository Instructions

## Local Command Execution

- Run PowerShell automation without loading the user profile, e.g.
  `powershell -NoProfile -ExecutionPolicy Bypass -File <script>.ps1`.

## Validation Commands

- Python tests: `py -3.12 -m pytest src/python/tests src/tests/python`
  (avoid root-level `pytest`; it can collect packaged copies under `Output/`).
- Rust tests: `cargo test --manifest-path src/rust/Cargo.toml`
- Rust checks:
  `cargo fmt --manifest-path src/rust/Cargo.toml --all -- --check`
  and `cargo clippy --manifest-path src/rust/Cargo.toml -p whisper-dictate-app --all-targets --all-features -- -D warnings`

## Regression Tests

When fixing a bug or changing performance-sensitive behavior, add the narrowest
useful regression test unless there is a clear technical reason not to:

- Unit tests for pure logic, parsing, configuration, command construction, and
  small platform guards.
- Integration or smoke tests when the bug is in process launch, installer
  behavior, runtime wiring, dependency setup, or cross-module behavior.
- Both when the bug has a small isolated cause and a higher-level workflow that
  could regress independently.

If a regression test is not practical, document the reason in the commit or PR
summary and include the manual verification that covers the bug.

## Pull request review

**HARD GATE — do not merge with unaddressed automated-review comments.**
CI green is not enough; fetch and triage Copilot/SonarCloud comments first.

- Before merging, wait for the Copilot review to land, then fetch comments:

  ```sh
  gh api repos/<owner>/<repo>/pulls/<pr>/comments \
    --jq '.[] | select(.user.login|test("copilot";"i")) | select(.in_reply_to_id==null) | "[\(.path):\(.line // .original_line)] \(.body)"'
  ```

  Fix each comment or record an explicit dismissal reason. Use
  `.line // .original_line` because outdated comments may have null `.line`.
- After pushing changes, re-request the review
  (`gh pr edit <pr> --add-reviewer @copilot`) and re-check before merging.
- Apply this gate to every PR, including scripted or batch merges.

## Model economy

For read-only information-gathering and simple mechanical comparisons (scanning files, looking up which secret holds which key, diffing across repos, summarizing configs), delegate to the cheapest *capable* sub-model your harness supports — Claude Code: the Task/Agent tool with Haiku or Sonnet; other harnesses: your equivalent, or skip if none. Keep design decisions, code edits, and irreversible actions on the primary model. Prefer correctness over economy — never use a model too weak for the task.

## Project-Specific Expectations

- Treat Windows as the primary supported desktop path. Changes to the Rust
  launcher/controller, installer, subprocess handling, console encoding,
  Settings UI behavior, and keyboard/text injection must be reviewed for
  Windows behavior, not just platform-neutral Python logic.
- Use the local installer loop for internal Windows testing. When changing
  installer files, shortcuts, bundled files, Rust UI/controller behavior, or
  Windows launch behavior, build a local installer with
  `scripts/windows/build-installer.ps1` and report the generated
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
  cmd.exe, hidden launchers, and Rust UI subprocess logs.
- Keep Whisper and Parakeet configuration separate. Parakeet must use its own
  model defaults, dependency checks, and CUDA readiness checks rather than
  inheriting Whisper model names such as `large-v3`.

