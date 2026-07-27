"""
Regression-test discipline scanner (AGENTS.md sections 32-58).

Fails CI when a PR adds/modifies substantial production code with zero
corresponding test additions. This is the analog of
``src/rust/console_ascii_tests.rs`` for the "Tests as a safety net"
rule: a discipline that was declared in AGENTS.md but not enforced
kept slipping past review, so encode it as a scanner and let CI catch
follow-ups the humans miss.

The rule the scanner enforces, paraphrased from AGENTS.md:

    "Any bug fix or behavior change should add the narrowest useful
     regression test. Flag PRs that change observable behavior with
     zero new test coverage."

Heuristic (deliberately narrow -- false-positive cost > false-negative):

1. Resolve the PR base ref -- ``GITHUB_BASE_REF`` on CI, ``origin/main``
   locally. If we cannot resolve one, or the resolved diff is empty
   (push-to-main after squash-merge), the test is a no-op.
2. For each production file that is Added / Modified / Renamed in the
   diff, drop trivia (blank lines, comments, doc-comments), then
   measure the added-line count. Below ``MIN_ADDED_LINES`` = a small
   refactor, exempt.
3. Scan the ADDED lines for NEW public symbols
   (``pub fn|struct|enum|trait|const|static`` on Rust,
    module-level ``def`` / ``class`` on Python). If none, the change is
   a private-refactor / internal-flow edit -- exempt.
4. If the file has a matching test file anywhere in the tree
   (``foo.rs`` -> ``foo_tests.rs`` / ``tests_foo.rs`` / ``tests/foo.rs``,
    ``foo.py`` -> ``test_foo.py`` / ``foo_tests.py``), exempt: the
   assumption is that the developer added tests there.
5. Otherwise, if the diff ADDED a test file that mentions any of the
   new public symbols by name, exempt.
6. Otherwise, FAIL and print a ``::error::`` line per offender.

Escape hatches:

- Explicit opt-out: including ``[no-test-required]`` anywhere in a
  commit message in the PR range disables the scanner for that PR. The
  tag is a documented reason -- Codex reviewers will still see it and
  can push back if it's abused.
- Test files, docs / workflows / config, scripts, examples, thin
  entrypoints (``main.rs`` / ``lib.rs`` / ``__init__.py``), the cargo
  build script, and the GUI entrypoint are exempt by path -- they are
  not what the discipline targets.

Why a Python test and not a Rust one: this must run on Python-only PRs
too, and it walks git history rather than source code (so
``proc_macro2`` buys us nothing). The Python ``unit`` job already
auto-picks up ``src/tests/python/test_*.py`` files.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]

# --- Tuning knobs ----------------------------------------------------

# A change with fewer NON-TRIVIA added lines than this is treated as a
# small refactor and never demands a test. 20 lines catches the "new
# feature quietly slipped in without coverage" case while ignoring
# rename/reflow churn. Bump if you're seeing false positives; lower
# with care -- the design principle is "miss a case rather than block
# a legit PR".
MIN_ADDED_LINES = 20

# The single-source per-PR opt-out. Documented in the failure message
# so a developer who hits the scanner discovers it without hunting.
OPT_OUT_TAG = "[no-test-required]"

# --- File classification --------------------------------------------

# Only these production extensions are governed by the discipline.
# Shell scripts, YAML, TOML etc are covered by the exempt-path list
# below anyway; this is an extra belt.
_GOVERNED_EXTENSIONS = (".rs", ".py")

# Path-prefix exemptions. Anything under these is NOT what "Tests as
# a safety net" targets: docs, workflows, generated packaging, build
# scripts, one-off tooling. Keep the list narrow -- a real production
# module under one of these prefixes would silently escape the check.
_EXEMPT_PATH_PREFIXES = (
    ".github/",
    "docs/",
    "assets/",
    "packaging/",
    "requirements/",
    "scripts/",
    "src/rust/examples/",
)

# Filename-level exemptions: thin entrypoints where a "test" would test
# argparse boilerplate rather than any real behavior. The unit smokes
# (``runtime --help``, Rust CLI smoke) cover these end-to-end.
_EXEMPT_FILENAMES = (
    "main.rs",
    "lib.rs",
    "__init__.py",
    "conftest.py",
    "setup.py",
    "build.rs",
    "whisper-dictate-gui.rs",
)


def _looks_like_test_file(path: str) -> bool:
    p = path.replace("\\", "/")
    name = p.rsplit("/", 1)[-1]
    if "/tests/" in p or "/test_support/" in p:
        return True
    if name.endswith("_tests.rs") or name.endswith("_tests.py") or name.endswith("_test.py"):
        return True
    if name.startswith("test_") or name.startswith("tests_"):
        return True
    if name == "tests.rs":
        return True
    return False


def _is_exempt(path: str) -> bool:
    p = path.replace("\\", "/")
    if not any(p.endswith(ext) for ext in _GOVERNED_EXTENSIONS):
        return True
    if any(p.startswith(prefix) for prefix in _EXEMPT_PATH_PREFIXES):
        return True
    name = p.rsplit("/", 1)[-1]
    if name in _EXEMPT_FILENAMES:
        return True
    if _looks_like_test_file(p):
        return True
    return False


# --- Public-symbol detectors ----------------------------------------

# Rust: any ``pub`` / ``pub(crate)`` / ``pub(super)`` item declaration
# for the item kinds that carry testable behavior. Deliberately DOES
# NOT match ``pub mod`` (a mod declaration is metadata) or ``pub use``
# (a re-export, tested by whatever it points to).
_RUST_PUBLIC = re.compile(
    r"^\s*pub(?:\s*\([^)]*\))?\s+(?:async\s+)?(?:unsafe\s+)?"
    r"(?:fn|struct|enum|trait|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)"
)

# Python: module-level ``def`` / ``class``. The strict start-of-line
# match keeps us from flagging methods inside classes (those go with
# the class, and Python has no per-method public/private distinction
# other than the leading-underscore convention we don't want to lean
# on here).
_PY_PUBLIC = re.compile(
    r"^(?:def|class)\s+([A-Za-z_][A-Za-z0-9_]*)"
)


def _new_public_symbols(added_lines: list[str], path: str) -> set[str]:
    pattern = _RUST_PUBLIC if path.endswith(".rs") else _PY_PUBLIC
    out: set[str] = set()
    for line in added_lines:
        m = pattern.match(line)
        if m:
            name = m.group(1)
            # Python leading-underscore convention: treat as private.
            if path.endswith(".py") and name.startswith("_"):
                continue
            out.add(name)
    return out


# --- Trivia stripping -----------------------------------------------

def _strip_trivia_rust(lines: list[str]) -> list[str]:
    out = []
    for raw in lines:
        s = raw.strip()
        if not s:
            continue
        # Line comments, block-comment openers, and continuation lines
        # in a doc-comment block that visually start with ``*``.
        if s.startswith("//") or s.startswith("/*") or s.startswith("*"):
            continue
        out.append(raw)
    return out


def _strip_trivia_python(lines: list[str]) -> list[str]:
    out = []
    for raw in lines:
        s = raw.strip()
        if not s:
            continue
        if s.startswith("#"):
            continue
        # Best-effort docstring skip. We can't do a real string-state
        # walk from a diff (we only see added lines), so drop lines
        # that look like docstring open/close markers. Middle lines of
        # a multi-line docstring slip through -- that pushes counts
        # UP toward MIN_ADDED_LINES, biased toward false positives on
        # trivia; that bias is fine because the ``_new_public_symbols``
        # check must ALSO fire before we flag the file.
        if s.startswith('"""') or s.startswith("'''"):
            continue
        out.append(raw)
    return out


def _substantive_added_lines(base: str, path: str) -> list[str]:
    added = _added_lines(base, path)
    if path.endswith(".rs"):
        return _strip_trivia_rust(added)
    if path.endswith(".py"):
        return _strip_trivia_python(added)
    return added


# --- Test-file matching ---------------------------------------------

def _candidate_test_names(prod_path: str) -> tuple[str, ...]:
    """Return the filenames that would count as the matching test file."""
    name = prod_path.rsplit("/", 1)[-1]
    if "." not in name:
        return ()
    stem, ext = name.rsplit(".", 1)
    if ext == "rs":
        return (
            f"{stem}_tests.rs",
            f"tests_{stem}.rs",
            f"{stem}.rs",  # only counted when it lives under a tests/ dir
        )
    if ext == "py":
        return (
            f"test_{stem}.py",
            f"{stem}_test.py",
            f"{stem}_tests.py",
        )
    return ()


def _find_matching_test_files(prod_path: str, all_files: list[str]) -> list[str]:
    candidates = _candidate_test_names(prod_path)
    if not candidates:
        return []
    prod_name = prod_path.rsplit("/", 1)[-1]
    matches: list[str] = []
    for path in all_files:
        name = path.rsplit("/", 1)[-1]
        if name not in candidates:
            continue
        # A `tests/foo.rs` shape only counts when it actually lives
        # under a directory named ``tests``. Otherwise ``foo.rs`` IS
        # the production file itself and would spuriously match.
        if name == prod_name:
            parts = path.split("/")
            if len(parts) < 2 or parts[-2] != "tests":
                if "/tests/" not in path:
                    continue
        matches.append(path)
    return matches


def _added_test_files_reference(diff_test_paths: list[str], symbols: set[str]) -> bool:
    if not symbols:
        return False
    for tf in diff_test_paths:
        try:
            text = (REPO_ROOT / tf).read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for sym in symbols:
            if not sym:
                continue
            if re.search(rf"\b{re.escape(sym)}\b", text):
                return True
    return False


# --- Git helpers ----------------------------------------------------

def _git(args: list[str], check: bool = True) -> str:
    try:
        proc = subprocess.run(
            ["git", *args],
            cwd=REPO_ROOT,
            check=check,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except FileNotFoundError:
        return ""
    if proc.returncode != 0 and not check:
        return ""
    return proc.stdout


def _rev_exists(ref: str) -> bool:
    return bool(_git(["rev-parse", "--verify", "--quiet", ref], check=False).strip())


def _resolve_base_ref() -> str | None:
    """Return the git ref to diff HEAD against, or None if not resolvable.

    On CI, prefer ``origin/$GITHUB_BASE_REF`` because ``actions/checkout``
    fetches the base under the origin namespace. Locally, prefer
    ``origin/main`` so we always compare against the pushed base rather
    than a stale local ``main`` branch. If nothing resolves (shallow
    clone with no origin refs, detached HEAD in a bare tree, ...), the
    caller skips.
    """
    ci_base = os.environ.get("GITHUB_BASE_REF", "").strip()
    if ci_base:
        for candidate in (f"origin/{ci_base}", ci_base):
            if _rev_exists(candidate):
                return candidate
    for candidate in ("origin/main", "main"):
        if _rev_exists(candidate):
            return candidate
    return None


def _diff_range(base: str) -> str:
    # Three-dot: only commits reachable from HEAD but not from base --
    # i.e. what THIS branch added. Two-dot would also include commits
    # base has that HEAD doesn't, which pollutes the change set on a
    # branch that hasn't rebased onto a moved main.
    return f"{base}...HEAD"


def _changed_files(base: str) -> dict[str, str]:
    """Return {path: status} for the diff range.

    Status is the first letter of ``git diff --name-status``' first
    field, so 'A' (added), 'M' (modified), 'R' (renamed), 'D'
    (deleted). Deleted files can't demand test coverage, so callers
    filter them out.
    """
    out = _git(["diff", "--name-status", _diff_range(base)])
    result: dict[str, str] = {}
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        status = parts[0].strip()[:1]
        # For a rename the last tab-field is the new path; for
        # add/modify/delete it's the only path field.
        path = parts[-1].strip().replace("\\", "/")
        if not path:
            continue
        result[path] = status
    return result


def _added_lines(base: str, path: str) -> list[str]:
    diff = _git([
        "diff",
        "--unified=0",
        "--no-color",
        _diff_range(base),
        "--",
        path,
    ], check=False)
    added: list[str] = []
    for line in diff.splitlines():
        # Header lines start with +++/---, hunk markers with @@.
        if line.startswith("+++") or line.startswith("---"):
            continue
        if line.startswith("@@"):
            continue
        if line.startswith("+"):
            added.append(line[1:])
    return added


def _commit_bodies_opt_out(base: str) -> bool:
    out = _git(["log", "--format=%B%x1e", _diff_range(base)], check=False)
    return OPT_OUT_TAG in out


def _all_repo_files() -> list[str]:
    out = _git(["ls-files"])
    return [p.replace("\\", "/") for p in out.splitlines() if p]


# --- The scanner ----------------------------------------------------

class RegressionTestDisciplineScanner(unittest.TestCase):
    """Fail CI when a substantial production change ships without a
    matching test addition. See module docstring for the full heuristic.
    """

    def test_added_production_code_has_matching_tests(self) -> None:
        base = _resolve_base_ref()
        if base is None:
            # Isolated environment with no origin/main reachable --
            # skip rather than fail. On CI actions/checkout always
            # provides at least one of the candidates, so a skip here
            # means we're running in an environment the scanner isn't
            # designed for.
            self.skipTest("no PR base ref reachable; scanner is a no-op here")

        changed = _changed_files(base)
        if not changed:
            # Push-to-main after squash-merge, or a run on the base
            # branch itself. Nothing to enforce.
            return

        if _commit_bodies_opt_out(base):
            print(
                f"::notice::regression-test scanner: commit message carries "
                f"{OPT_OUT_TAG}; skipping",
                file=sys.stderr,
            )
            return

        # Test files added by the diff. Used to satisfy step 5 of the
        # heuristic: a new module + a new test file that references
        # its API is exactly the pattern we want to allow, even if the
        # test file lives under an unconventional path.
        added_test_paths = [
            path for path, status in changed.items()
            if status in ("A", "M") and _looks_like_test_file(path)
        ]

        all_files = _all_repo_files()

        violations: list[str] = []
        for path, status in sorted(changed.items()):
            if status not in ("A", "M", "R"):
                continue
            if _is_exempt(path):
                continue

            added = _substantive_added_lines(base, path)
            if len(added) < MIN_ADDED_LINES:
                continue

            new_symbols = _new_public_symbols(added, path)
            if not new_symbols:
                # Substantial diff but no NEW public API surface --
                # an internal refactor. Not what the discipline is
                # for (see AGENTS.md: "changes observable behavior").
                continue

            matching_tests = _find_matching_test_files(path, all_files)
            if matching_tests:
                continue

            if _added_test_files_reference(added_test_paths, new_symbols):
                continue

            candidates = _candidate_test_names(path)
            violations.append(
                f"{path}: added {len(added)} non-trivia lines introducing "
                f"public symbol(s) {sorted(new_symbols)!r} but no matching "
                f"test file exists (looked for: {list(candidates)}) and no "
                f"added test file in this PR references those symbols by name. "
                f"AGENTS.md 'Tests as a safety net' asks for a regression test "
                f"alongside behavior changes. Add one under src/rust/*_tests.rs "
                f"(Rust) or src/python/tests/test_*.py (Python), OR if the "
                f"change genuinely cannot be tested (e.g. an OS-integration "
                f"glue file), include '{OPT_OUT_TAG}' with a reason in a "
                f"commit message."
            )

        if violations:
            report = "\n".join(f"::error::{v}" for v in violations)
            self.fail(
                "Regression-test discipline (AGENTS.md sections 32-58): the "
                "following production files added public API surface without "
                "a matching test addition. This scanner is the analog of "
                "console_ascii_tests.rs for the 'Tests as a safety net' rule "
                "-- the discipline was on paper only until this landed.\n\n"
                + report
            )


if __name__ == "__main__":
    unittest.main()
