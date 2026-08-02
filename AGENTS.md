# Repository Instructions

## Repository layout

- The shipped application and runtime are under `src/rust`.
- User-facing documentation is under `docs/`; engineering, release, and test
  procedures belong under `docs/dev/`.
- Installer and distribution inputs are under `packaging/`; CI and workflow
  definitions are under `.github/workflows/`.
- Cross-platform developer and smoke-test automation is under `scripts/`.

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
- **Native clippy is broken on Windows boxes without MSVC `lib.exe`.**
  Use `pwsh scripts/dev/dev-check.ps1` instead — it runs the CI rust
  matrix's **Linux leg** (fmt + clippy + default test + feature-gated
  tests for rust-hotkeys / audio-in-rust + whisper-rs-local build + CLI
  smoke) inside the devcontainer image via Rancher Desktop's Docker
  `default` context. The image is built once on first run
  (~5 min); thereafter Docker's layer cache keeps subsequent runs
  near-instant. Requires Rancher Desktop with its container engine running. Run this BEFORE every
  signed push so clippy lints fail locally rather than costing a CI
  roundtrip. NOTE: the `rust (windows-2025)` matrix leg is NOT covered
  — for Windows-specific code (tray, input injection on Win32, WASAPI
  capture, etc.) lean on CI to catch Windows-only regressions.
  `-SkipExtraFeatures` limits the run to the default-feature leg only
  (fast iteration when feature paths are untouched).
  `-DryRun` prints every cargo argv without invoking docker.

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

## Code Review Rules

Guidance for automated reviewers (Codex, Copilot, etc.) reviewing pull
requests in this repository. Comment only on findings that match one of the
categories below; skip stylistic preferences not encoded here.

- **Modularity.** No new files over ~500 lines and no oversized methods —
  split into modules + small helpers so each piece stays unit-testable.
- **Root documentation.** Keep root-level user documents such as `README.md`
  and `CONTRIBUTING.md` current and user-facing; engineering procedures belong
  under `docs/dev/`.
- **Nix entry point.** Changes to root `flake.nix` must preserve the supported
  Rust CLI/UI launch paths and keep its imported package payload current.
- **Shared settings schema.** Changes to `shared/config/settings_schema.json`
  must update generated configuration documentation and include focused schema
  or configuration coverage.
- **Tests as a safety net.** Any bug fix or behavior change should add the
  narrowest useful regression test (unit for pure logic, integration/smoke
  for process launch, installer, runtime wiring, or cross-module flows).
  Flag PRs that change observable behavior with zero new test coverage.
- **Windows-first.** Treat Windows as the primary supported desktop. Flag
  changes to the Rust launcher/controller, installer, subprocess handling,
  console encoding, Settings UI behavior, or keyboard/text injection that
  ship without Windows-specific verification.
- **Console output is ASCII- or UTF-8-safe.** New stdout/stderr lines, log
  messages, and installer scripts must work under PowerShell, cmd.exe,
  hidden launchers, and the Rust UI's subprocess logs. Non-ASCII without a
  tested fallback is a defect.
- **Speech-to-text choices stay current.** The supported engine values are
  `whisper` and `openai`; cloud providers are `openai`, `groq`, and `custom`.
  Do not add retired backend selectors, provider-specific legacy environment
  variables, or removed model dependencies. Existing legacy config values may
  be migrated at load time when compatibility requires it.
- **Dictionary/prompt changes stay bounded.** Any change to dictionary
  loading, prompt construction, term selection, or replacements must
  preserve prompt length caps and include tests for `terms` AND
  `replacements` behavior.
- **Preserve the unified controller model.** The Rust UI owns the managed
  runtime process on Windows. Don't introduce duplicate UI instances or
  silent restarts — start/stop/restart must be explicit and visible.
- **Cargo features stay off by default.** New experimental features should
  be opt-in via cargo features or env vars so the default install stays
  unchanged.
- **Secrets and tokens.** Never accept hardcoded API keys, tokens, or
  account-bound URLs in tracked code. Flag any `.env`-style file added
  without a corresponding ignore entry.
- **PR scope discipline.** Flag PRs that bundle unrelated changes (a bug
  fix + a drive-by refactor + new dependencies) — they should be split.
- **Many small PRs beat one big one.** Default to the smallest change that
  stands on its own and ship it; open a follow-up for the rest. A large PR
  costs more than it looks: it sits in review, collects automated-review
  rounds on unrelated hunks, drifts against `main`, and is harder to revert
  when one part of it turns out wrong. Splitting is the norm, not the
  exception — reach for it before the PR is written, not after a reviewer
  asks. Two rules of thumb: if the description needs the word "and" to say
  what the PR does, it is probably two PRs; and if half the change is
  independently mergeable and useful, merge that half now.
  The exception is a fix that would leave the tree in a worse or
  half-broken state if only part of it landed — a security fix that spans
  two components, say. Keep those together and say why in the description.

When suggesting fixes, prefer the smallest change that addresses the
finding; do not propose refactors beyond what the PR's stated scope
requires.

## Pull request review

**HARD GATE — do not merge with unaddressed automated-review comments.**
CI green is not enough; fetch and triage Claude / Codex / Copilot /
SonarCloud comments first.

Claude Code agents: `.claude/skills/pr-gate/SKILL.md` is the executable form
of this section — ready-to-run queries, the settle window, base-branch
verification, and the post-merge sweep. Invoke it rather than reconstructing
the procedure from this prose.

**Claude is the primary auto-reviewer** (configured in
`.github/workflows/claude-review.yml`). One round per PR, fired on
`pull_request: opened`. If a developer wants a fresh Claude pass after
pushing fixes, they `@claude` mention in a PR comment (handled by
`claude.yml`).

Automated review availability can change independently of CI. Treat any
automated review as feedback that must be triaged when it appears, and keep
waits for optional reviewers bounded so the merge process remains safe if a
reviewer does not run.

- Before merging, wait for the auto-review to land (Claude typically
  posts within 5-10 minutes of the workflow firing). Fetch ALL inline
  comments — use `--paginate` because `per_page` defaults to 30 and a
  busy PR easily exceeds that:

  ```sh
  gh api --paginate repos/<owner>/<repo>/pulls/<pr>/comments \
    --jq '.[] | select(.user.login | test("claude|codex|copilot|sonar"; "i")) | select(.in_reply_to_id == null) | "[\(.path):\(.line // .original_line)] \(.body)"'
  ```

  Use `.line // .original_line` because outdated comments may have null
  `.line`. The login filter covers Claude, Codex, Copilot, AND
  SonarCloud (whose inline comments come from `sonarqubecloud[bot]`)
  — all four are auto-review sources gated by this rule.

- **For EVERY inline review comment, before merging, do all three:**
  1. **Fix or explicitly decline** the suggestion (push a follow-up commit, or
     post a reply explaining why it's not actionable / a false positive).
  2. **Mark the thread resolved** via the GraphQL `resolveReviewThread`
     mutation:

     ```sh
     gh api graphql -f query='mutation { resolveReviewThread(input: { threadId: "PRRT_..." }) { thread { isResolved } } }'
     ```

     Get thread ids via the paginated GraphQL query below. A PR with
     more than 50 review threads needs cursor pagination
     (`pageInfo { hasNextPage endCursor }` + `after: "..."`); the
     example below shows the first-page form and you must loop on
     `hasNextPage`. Always select BOTH `line` and `originalLine` so
     outdated threads (whose `line` may be null) still match the comment
     they belong to:

     ```sh
     gh api graphql -f query='
       query($owner: String!, $name: String!, $pr: Int!, $cursor: String) {
         repository(owner: $owner, name: $name) {
           pullRequest(number: $pr) {
             reviewThreads(first: 50, after: $cursor) {
               pageInfo { hasNextPage endCursor }
               nodes {
                 id
                 isResolved
                 comments(first: 1) {
                   nodes { databaseId path line originalLine }
                 }
               }
             }
           }
         }
       }' -F owner=... -F name=... -F pr=...
     ```

  3. **React with 👍 or 👎** on the original comment so the reviewer can
     score signal quality going forward:

     ```sh
     # 👍 if the finding was a real bug we fixed
     gh api repos/<owner>/<repo>/pulls/comments/<comment_id>/reactions -f content='+1'
     # 👎 if the finding was a false positive / we explicitly decided not to act
     gh api repos/<owner>/<repo>/pulls/comments/<comment_id>/reactions -f content='-1'
     ```

  The fix-reply with the resolving commit SHA goes on the same thread via
  `POST /repos/<owner>/<repo>/pulls/<pr>/comments` with
  `in_reply_to=<comment_id>` so the audit trail stays inline. Apply to
  every PR including admin-merged dependency bumps.

- Claude review is **one round per PR** (`pull_request: opened` only).
  After pushing fixes the auto-pass does NOT re-run automatically; if
  a fresh pass is genuinely useful (e.g. a substantial rewrite during
  Codex/Copilot iteration), `@claude` mention in a PR comment fires
  `claude.yml` for an on-demand review.

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
- Speech-to-text configuration remains native and current: use `whisper` or
  `openai` as the engine and `openai`, `groq`, or `custom` as cloud providers.
  Legacy values may be migrated at load time, but new code must not add old
  runtime selectors or removed model dependencies.
