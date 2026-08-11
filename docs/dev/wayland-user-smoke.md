# Wayland user smoke script

`scripts/integration/wayland-user-smoke.sh` is the **one script** the
maintainer runs on the Ubuntu 26.04 Wayland ThinkPad after installing each
new whisper-dictate release. It exercises every shipped user-facing
feature headlessly and prints a pass/fail/skip summary in under a minute.

## Why this exists

The user does not want to remember which flags/subcommands/features shipped
in which release. Their contract with the project is:

> "Clone (or pull), run one script, paste the summary. That is the entire
> manual verification step."

Everything the app claims to do should be reachable from that script.
If something is not verifiable headless yet, the section warn-skips and
carries a `pending audit item 2` note so it stays visible.

## Rules for feature PRs (BLOCKING)

Every PR that adds or changes a user-facing feature MUST touch
`scripts/integration/wayland-user-smoke.sh` in the same PR. Even a
one-liner that runs `<verb> --help` and greps for a known flag is enough
to prove the surface is wired up.

Concretely, add or update a section for:

- **New CLI subcommand or flag** — invoke it (with `--help` at minimum,
  or a real dry-run if it has one), assert exit 0, grep for expected
  output.
- **New injection backend** — invoke its `--dry-run` mode, assert on
  the preview line.
- **New model catalog entry** — extend the `models list` grep.
- **New config surface** — `config <verb>` invocation.
- **New hotkey behaviour** — for now the listener-startup smoke; when a
  `--verify-hotkey-only` (or equivalent) flag ships, wire the real check
  in and drop the warn.

If a feature genuinely cannot be exercised headlessly yet (e.g. real
microphone capture, real Ctrl+V paste into a live focused window), add a
section that `warn`s with a clear reason. The `warn` count is visible in
the summary, so the missing coverage does not silently disappear.

Coordinator responsibility: every merged feature PR is checked against
this rule; if the smoke script was not updated, open a follow-up PR
adding the missing section.

## How to run

From the repo root, on the Wayland box:

```bash
bash scripts/integration/wayland-user-smoke.sh
```

The script requires the installed native `whisper-dictate` binary on
`$PATH`; this is the release-verification path. The detected binary is
printed in the **Environment** section, and the script stops with an
actionable error if it is missing.

The Environment section also prints a **command origin**: `release` for a
prebuilt/shipped artifact and `source-install` for a binary the developer
built locally (`scripts/linux/install-rust-ui.sh` compiling from a checkout,
or a `target/release` build on `$PATH`). The distinction matters because that
installer builds the full native dictation feature set, while the
release workflow enables the canonical `shipping` Cargo feature profile.
Sections that treat a missing cargo feature as a **packaging regression** —
today the `self-test hotkey-boot` rebuild-with branch — fail only when the
origin is `release`; on a `source-install` the same message is the documented,
expected skip. An unrecognised layout is classified `release` on purpose, so a
genuinely broken release still fails loudly.

Exit code: `0` if no section failed, non-zero otherwise. Skips do
not fail the run.

## What to do when a section fails

1. Re-run with `bash -x scripts/integration/wayland-user-smoke.sh 2>&1
   | tee /tmp/wayland-smoke.log` to get the full trace.
2. Paste `/tmp/wayland-smoke.log` (or just the failing section) back to
   the coordinator; they investigate before shipping the next feature
   or cutting the next release.

## What "warn / skipped" means

The section is intentionally in the script but the feature is not yet
verifiable from a headless Wayland session. Skips are safe (they do not
fail the run) but they DO show up in the final `Skipped:` count so no
one loses track. When the underlying capability ships, replace the
`warn` with a real check in the same PR.

## Related

- Project memory rule: `wayland-smoke-script-per-feature`.
- Ubuntu 26.04 CI container:
  `.github/workflows/` (see #490) — same OS baseline as the ThinkPad, so
  green CI + green local smoke is the shipping bar.
- The smoke script verifies the current `simulate-session` and `dictate-mic`
  help surfaces; a cloud key is required for a real end-to-end drive.
