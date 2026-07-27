"""Regression guard: the Groq CLI smoke script must never put an API key on
argv.

Backfills the Codex P2 finding on PR #597 ("Add regression coverage for
keeping the key out of argv"). PR #597 fixed the leak (a Groq key was on the
`cargo run cloud-transcribe --api-key ...` command line AND on a python-heredoc
positional argv), but the smoke workflow only asserts the script exits 0, so a
future refactor could silently reintroduce either exposure and CI would stay
green.

A command line is readable by other local users (`ps aux`,
`/proc/<pid>/cmdline`; Linux only hides it under `hidepid`, which is not the
default), so we pin the shape of the script itself — every invocation must
pass the key through the environment, never as a flag or positional arg.
"""
from __future__ import annotations

import re
import unittest
from pathlib import Path


SMOKE = Path("scripts/integration/groq-cli-smoke.sh")


def _code_lines(text: str) -> list[str]:
    """Lines with `# ...` trailing comments stripped and pure-comment lines
    dropped. Bash has no multi-line comments, so a per-line strip suffices.

    Handles the common case where a `#` inside a string is quoted; the smoke
    script has none, so a naive first-`#` split is safe here. Kept narrow on
    purpose — a fully general bash tokeniser would obscure the assertion.
    """
    out: list[str] = []
    for raw in text.splitlines():
        stripped = raw.lstrip()
        if not stripped or stripped.startswith("#"):
            continue
        # Trailing `# ...` — split on the first unquoted `#`. The script has
        # no `#` inside strings today; if it grows one, tighten this helper.
        idx = raw.find("#")
        if idx != -1:
            # Only treat as a comment when preceded by whitespace (so `#!`
            # style tokens and array indices survive).
            if idx == 0 or raw[idx - 1].isspace():
                raw = raw[:idx].rstrip()
        if raw.strip():
            out.append(raw)
    return out


class GroqCliSmokeNoArgvKey(unittest.TestCase):
    def setUp(self) -> None:
        # The test is repo-root relative — mirror the pattern already used by
        # src/tests/python/test_release_workflows.py.
        self.script_text = SMOKE.read_text(encoding="utf-8")
        self.code = _code_lines(self.script_text)

    def test_script_exists_where_the_workflow_expects_it(self):
        # If someone renames or deletes the script, this test fails first and
        # names the file — better than a mysterious grep-nothing regression
        # elsewhere.
        self.assertTrue(SMOKE.is_file(), f"missing: {SMOKE}")

    def test_no_api_key_flag_on_any_command_invocation(self):
        # The `--api-key` flag on argv is exactly what PR #588 (Python worker)
        # and PR #597 (this script) removed. A comment mentioning the flag is
        # fine (and desirable — it documents the fix), so we scan only code
        # lines. Match `--api-key` as a whole token so a future variant like
        # `--api-key-file` would still be allowed if someone deliberately
        # adopts it (path arg, not the secret itself).
        pattern = re.compile(r"(?<![\w-])--api-key(?![\w-])")
        offenders = [line for line in self.code if pattern.search(line)]
        self.assertEqual(
            offenders,
            [],
            "smoke script must never put --api-key on a command line "
            "(readable via `ps aux` / `/proc/<pid>/cmdline`); found: "
            f"{offenders}",
        )

    def test_key_is_passed_via_env_var(self):
        # The env-var handoff is the fix. Pin it so a rewrite that reverts to
        # argv fails visibly here rather than "just" reintroducing the leak.
        # `VOICEPI_STT_API_KEY=` is the assignment form; anywhere it appears
        # in the code (not comments) proves the env path is wired.
        assignments = [line for line in self.code if "VOICEPI_STT_API_KEY=" in line]
        self.assertGreaterEqual(
            len(assignments),
            1,
            "smoke script must pass the Groq key via VOICEPI_STT_API_KEY env "
            "var — that's the env-only handoff PR #597 established.",
        )

    def test_groq_api_key_reference_never_appears_as_positional_argv(self):
        # PR #597's second exposure was a python heredoc invoked as
        # `"$PY" - "$transcript" "$GROQ_API_KEY" "$GROQ_BASE" ...` — the key
        # was on argv of the *builder* process, not the CLI. Now the builder
        # reads it from `os.environ["GROQ_API_KEY"]`.
        #
        # Whitelist the safe shapes we actually use so any new appearance of
        # `"$GROQ_API_KEY"` or `$GROQ_API_KEY` is flagged for review:
        #   - `VAR="$GROQ_API_KEY"` — env-var assignment (safe; the child
        #     process sees it in its environment, not its argv)
        #   - `[[ -z "${GROQ_API_KEY:-}" ]]` etc. — brace-form parameter
        #     expansion for the skip-when-absent check (safe; not a subprocess
        #     invocation)
        #
        # Anything else — a bare `"$GROQ_API_KEY"` as a positional argument to
        # a command — is the class of leak Codex flagged.
        key_ref = re.compile(r'"\$GROQ_API_KEY"|\$GROQ_API_KEY(?![_A-Za-z])')
        env_assign = re.compile(r'[A-Z_][A-Z0-9_]*="\$GROQ_API_KEY"')
        brace_form = re.compile(r'\$\{GROQ_API_KEY[:\-\}]')
        offenders: list[str] = []
        for line in self.code:
            if not key_ref.search(line):
                continue
            # Every occurrence in this line must be part of a safe shape.
            residue = env_assign.sub("", line)
            residue = brace_form.sub("", residue)
            if key_ref.search(residue):
                offenders.append(line)
        self.assertEqual(
            offenders,
            [],
            "GROQ_API_KEY must only appear as `VAR=\"$GROQ_API_KEY\"` "
            "(env-var handoff) or `${GROQ_API_KEY...}` (parameter expansion); "
            "passing it as a positional argv leaks it into `ps` listings. "
            f"Offenders: {offenders}",
        )


if __name__ == "__main__":  # pragma: no cover - manual runner
    unittest.main()
