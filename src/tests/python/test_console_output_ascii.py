"""Console ASCII guard for the Python and installer surfaces.

AGENTS.md names three: "New stdout/stderr lines, log messages, and installer
scripts must work under PowerShell, cmd.exe, hidden launchers, and the Rust
UI's subprocess logs."

#576 enforced the rule for Rust. This covers the other two. The violations
themselves were fixed first, in #589 -- this is enforcement only, so a failure
here means something new was added rather than something old was found.

ONE BLOCKLIST, THREE SURFACES
-----------------------------
The character list is NOT duplicated here. It is parsed out of the Rust
guard's `TYPOGRAPHIC` const, so the three surfaces cannot drift into
disagreeing about which characters are safe. Two copies that disagree is a
guard that passes on one surface and fails on another for the same character,
which is worse than having no guard on the second surface at all.

WHY `ast` AND NOT A REGEX
-------------------------
Python string literals have the same traps Rust's do -- f-strings, triple
quotes, raw and byte prefixes, implicit concatenation across lines. The Rust
guard's history is five review rounds of a hand-rolled lexer losing to
exactly that, so this uses the real parser from the start.

WHY NOT KEYED ON `print()`
--------------------------
The first cut of the #589 fixes matched only string literals that were direct
arguments to `print()`, and review found it missed a message built one hop
away (a helper returned it, an exception carried it, the caller printed it).
Call-site literals turn out to be the minority of console text here. So this
walks every non-docstring literal and carries a narrow allowlist instead.
"""

from helpers import (
    Path,
    unittest,
)
import ast
import re

# Where the single source of truth lives.
_RUST_GUARD = Path("src/rust/console_ascii_tests.rs")

# `('\u{2014}', "-"),  // em dash`
_RUST_ENTRY = re.compile(r"\('\\u\{([0-9A-Fa-f]{4,6})\}',\s*\"((?:[^\"\\]|\\.)*)\"\)")

# Modules whose strings can reach a console. `whisper_dictate/` is the
# shipping package; its tests live elsewhere and reach a test runner.
_PYTHON_ROOT = Path("src/python/whisper_dictate")

# Installer scripts, which AGENTS.md names explicitly.
#
# `scripts/integration/**` is deliberately NOT covered. Those are developer
# tooling run by maintainers, not shipped to users, and the one with blocked
# characters (`wayland-user-smoke.sh`) is a Wayland-only Linux script that can
# never run on a cmd.exe code page. Extending the guard there would be
# enforcing a Windows console rule against a script that has no Windows
# console -- and it is under active edit by another session.
_SHELL_ROOTS = [Path("packaging")]

_SHELL_OUTPUT = re.compile(r"\b(echo|printf)\b")

# Exemptions scoped to a specific literal, never a whole file, each with the
# reason it is not console output. Mirrors the Rust guard's ALLOWLIST.
_ALLOWLIST = [
    (
        "src/python/whisper_dictate/vp_postprocess.py",
        "bliver til",
        "regex that MATCHES a user typing an arrow in a dictionary prompt -- "
        "input parsing, not output. Dropping it would silently stop "
        "recognising prompts people already write. The Rust guard carries the "
        "same exemption for postprocess/prompt.rs.",
    ),
]


def _blocklist() -> dict[str, str]:
    """Parse the blocked characters out of the Rust guard.

    Deriving rather than duplicating is the whole point -- see the module
    docstring. A parse that silently returned nothing would make every test
    below pass vacuously, so the count is asserted.
    """
    src = _RUST_GUARD.read_text(encoding="utf-8")
    found = {
        chr(int(code, 16)): ascii_form.replace('\\"', '"')
        for code, ascii_form in _RUST_ENTRY.findall(src)
    }
    if len(found) < 15:
        raise AssertionError(
            f"only parsed {len(found)} characters out of {_RUST_GUARD}; the "
            "const's shape must have changed. Fix this parser rather than "
            "copying the list, or the three surfaces will drift."
        )
    return found


BLOCKED = _blocklist()


def _is_exempt(rel: str, text: str) -> bool:
    return any(rel == path and needle in text for path, needle, _ in _ALLOWLIST)


def _python_violations():
    """Every non-docstring string literal in the shipping package."""
    violations = []
    for path in sorted(_PYTHON_ROOT.rglob("*.py")):
        rel = path.as_posix()
        src = path.read_text(encoding="utf-8")
        tree = ast.parse(src, filename=rel)

        # Docstrings are developer documentation and never printed. Collect
        # them by identity so an identical string used elsewhere still counts.
        docstrings = set()
        for node in ast.walk(tree):
            if isinstance(
                node,
                (ast.Module, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef),
            ):
                first = node.body[0] if node.body else None
                if (
                    isinstance(first, ast.Expr)
                    and isinstance(first.value, ast.Constant)
                    and isinstance(first.value.value, str)
                ):
                    docstrings.add(id(first.value))

        for node in ast.walk(tree):
            if not isinstance(node, ast.Constant) or not isinstance(node.value, str):
                continue
            if id(node) in docstrings or _is_exempt(rel, node.value):
                continue
            for bad in sorted(set(node.value) & set(BLOCKED)):
                snippet = " ".join(node.value.split())[:60]
                violations.append(
                    f"{rel}:{node.lineno}: {bad!r} (use {BLOCKED[bad]!r}) in {snippet!r}"
                )
    return violations


def _shell_violations():
    """`echo` / `printf` lines in the installer scripts.

    Line-oriented on purpose: there is no shell parser to lean on the way
    `ast` and `proc_macro2` are leaned on elsewhere, and pretending otherwise
    would repeat the hand-rolled-lexer mistake. The trade is stated rather
    than hidden -- this misses a glyph built into a variable and expanded
    later, and it is why the installer's status markers were centralised into
    `ok()` / `info()` / `warn()` where one line covers every use.
    """
    violations = []
    for root in _SHELL_ROOTS:
        for path in sorted(root.rglob("*.sh")):
            rel = path.as_posix()
            for lineno, line in enumerate(
                path.read_text(encoding="utf-8").splitlines(), 1
            ):
                if not _SHELL_OUTPUT.search(line):
                    continue
                for bad in sorted(set(line) & set(BLOCKED)):
                    violations.append(
                        f"{rel}:{lineno}: {bad!r} (use {BLOCKED[bad]!r}) in "
                        f"{line.strip()[:60]!r}"
                    )
    return violations


class ConsoleOutputAsciiTests(unittest.TestCase):
    def test_python_console_strings_are_ascii_safe(self):
        violations = _python_violations()
        self.assertEqual(
            violations,
            [],
            "Typographic punctuation in Python strings that can reach "
            "stdout/stderr; these garble under cmd.exe on a legacy code page "
            "(AGENTS.md). Replace with the ASCII equivalent shown:\n"
            + "\n".join(violations),
        )

    def test_installer_scripts_are_ascii_safe(self):
        violations = _shell_violations()
        self.assertEqual(
            violations,
            [],
            "Typographic punctuation in installer script output:\n"
            + "\n".join(violations),
        )

    def test_the_scan_actually_reaches_the_sources(self):
        """Guard against a silent zero.

        A path typo or a moved directory would make both tests above pass
        while checking nothing -- the exact failure mode the Rust guard was
        rewritten to eliminate, so it gets pinned here from the start.
        """
        py = list(_PYTHON_ROOT.rglob("*.py"))
        self.assertGreater(len(py), 20, f"only found {len(py)} Python modules")
        sh = [p for root in _SHELL_ROOTS for p in root.rglob("*.sh")]
        self.assertGreater(len(sh), 0, "found no installer scripts")

    def test_blocklist_is_shared_with_the_rust_guard(self):
        """The three surfaces must agree on which characters are unsafe."""
        self.assertIn("\u2014", BLOCKED, "em dash missing from the shared list")
        self.assertEqual(BLOCKED["\u2014"], "-")
        self.assertIn("\u2713", BLOCKED, "check mark missing from the shared list")

    def test_blocklist_leaves_real_language_data_alone(self):
        """The property that makes a blocklist safer than an is_ascii() sweep.

        Most non-ASCII in this tree is legitimate and must keep working:
        Danish hallucination phrases, per-locale keymap data, dictionary
        stopwords, casefold fixtures. If the blocklist ever caught any of it,
        the guard would be turned off rather than obeyed.

        An earlier version of this test asserted "no character in the list is
        alphabetic", which is the right intent and the wrong check: Python
        counts letterLIKE symbols such as U+2139 INFORMATION SOURCE as
        alphabetic, so it failed on an entry that is perfectly safe to block.
        Asserting the actual property beats asserting a proxy for it.
        """
        samples = [
            "gaa ned ad gaden med aeblegroed",  # ASCII baseline
            "sm\u00f8rrebr\u00f8d p\u00e5 \u00c6r\u00f8",  # Danish
            "Stra\u00dfe",  # German casefold fixture
            "\u041f\u0440\u0438\u0432\u0435\u0442",  # Cyrillic keymap data
            "za\u017c\u00f3\u0142\u0107 g\u0119\u015bl\u0105",  # Polish
            "\u00e9\u00e8\u00ea\u00ab\u00bb",  # French, incl. guillemets
        ]
        for text in samples:
            hits = sorted(set(text) & set(BLOCKED))
            self.assertEqual(hits, [], f"blocklist must not touch {text!r}: {hits}")

    def test_planted_violations_are_detected(self):
        """The scanners must actually find what they claim to.

        Without this, a bug in the AST walk or the line filter makes both
        sweeps vacuously green -- which is how 30 violations accumulated on
        the Rust side before #576.
        """
        planted = ast.parse('print("bad \\u2014 output")')
        found = [
            c
            for node in ast.walk(planted)
            if isinstance(node, ast.Constant) and isinstance(node.value, str)
            for c in node.value
            if c in BLOCKED
        ]
        self.assertEqual(found, ["\u2014"], "escaped em dash must decode and be seen")

        self.assertTrue(
            _SHELL_OUTPUT.search('echo "  \u2713 done"'),
            "shell output matcher must see an echo line",
        )
        self.assertFalse(
            _SHELL_OUTPUT.search("  local x=1"),
            "shell output matcher must not fire on ordinary lines",
        )

    def test_allowlist_entries_still_match_a_real_literal(self):
        """A stale exemption silently widens the guard."""
        for path, needle, reason in _ALLOWLIST:
            self.assertTrue(reason.strip(), f"{path} needs a reason")
            p = Path(path)
            self.assertTrue(p.is_file(), f"allowlisted file is gone, drop it: {path}")
            src = p.read_text(encoding="utf-8")
            self.assertIn(
                needle,
                src,
                f"allowlist needle {needle!r} no longer appears in {path} -- "
                "delete the entry",
            )
