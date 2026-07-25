"""Guard: no typographic non-ASCII in Rust strings that reach a console.

AGENTS.md: "Console output is ASCII- or UTF-8-safe. New stdout/stderr lines,
log messages, and installer scripts must work under PowerShell, cmd.exe,
hidden launchers, and the Rust UI's subprocess logs. Non-ASCII without a
tested fallback is a defect."

That rule was declared but unenforced, so it leaked: 28 production strings
carried em dashes and arrows, and a fresh one slipped into #574 before review
caught it. cmd.exe on a legacy code page renders those as mojibake.

WHY A BLOCKLIST, NOT A BLANKET ASCII CHECK
------------------------------------------
Most non-ASCII in this codebase is legitimate and must keep working:
Danish hallucination phrases, per-locale keyboard layout data (aeoe/Cyrillic
/Polish), dictionary stopwords, and `Strasse` casefold fixtures. A blanket
`is_ascii()` sweep would flag all of it.

So this checks a small set of TYPOGRAPHIC PUNCTUATION only. It contains no
letters, which is what makes it safe: every language-data string above passes
untouched, while the characters people actually reach for when writing prose
in an error message are caught. Those have plain ASCII equivalents that cost
nothing -- "-" for an em dash, "->" for an arrow.
"""

from helpers import (
    Path,
    unittest,
)
import re

# Punctuation only -- deliberately no letters. See the module docstring.
TYPOGRAPHIC = {
    "—": "-",  # em dash
    "–": "-",  # en dash
    "→": "->",  # rightwards arrow
    "←": "<-",  # leftwards arrow
    "“": '"',  # left double quote
    "”": '"',  # right double quote
    "‘": "'",  # left single quote
    "’": "'",  # right single quote
    "…": "...",  # ellipsis
    "≤": "<=",
    "≥": ">=",
    "≠": "!=",
    "×": "x",  # multiplication sign
    "•": "-",  # bullet
}

# Files exempt from the rule, each with the reason it does not apply.
ALLOWLIST = {
    # A regex that MATCHES a user typing "->" or an arrow in a dictionary
    # prompt. This is input parsing, not console output -- dropping the arrow
    # would silently stop recognising prompts people already write.
    "src/rust/postprocess/prompt.rs": "regex matches user-typed arrows (input, not output)",
}


def _is_test_source(rel: str, src: str) -> bool:
    """True for files whose strings never reach a real console."""
    name = rel.rsplit("/", 1)[-1]
    if name.endswith("_tests.rs") or name == "tests.rs" or name.startswith("tests_"):
        return True
    if "/tests/" in rel or "test_support" in rel:
        return True
    # `#![cfg(test)]` as an inner attribute makes the whole file test-only.
    return bool(re.search(r"^#!\[cfg\(test\)\]", src, re.M))


def _string_literals(src: str):
    """Yield (line, literal) for each Rust string literal, skipping comments.

    Hand-rolled rather than regex-based because Rust raw strings (`r"..."`,
    `r#"..."#`) do not honour backslash escapes, and a naive regex either
    terminates them early or swallows the rest of the file.
    """
    i, n, line = 0, len(src), 1
    while i < n:
        ch = src[i]
        if ch == "\n":
            line += 1
            i += 1
            continue
        raw = re.match(r'r(#*)"', src[i:])
        if raw and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            closing = '"' + raw.group(1)
            end = src.find(closing, i + len(raw.group(0)))
            end = n if end < 0 else end + len(closing)
            literal = src[i:end]
            yield line, literal
            line += literal.count("\n")
            i = end
            continue
        if ch == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    break
                j += 1
            literal = src[i : j + 1]
            yield line, literal
            line += literal.count("\n")
            i = j + 1
            continue
        if src.startswith("//", i):
            nl = src.find("\n", i)
            i = n if nl < 0 else nl
            continue
        if src.startswith("/*", i):
            close = src.find("*/", i)
            close = n if close < 0 else close + 2
            line += src[i:close].count("\n")
            i = close
            continue
        i += 1


class ConsoleOutputAsciiTests(unittest.TestCase):
    def test_no_typographic_punctuation_in_console_strings(self):
        violations = []
        for path in sorted(Path("src/rust").rglob("*.rs")):
            rel = path.as_posix()
            if rel in ALLOWLIST:
                continue
            # `src/rust/ui/**` renders through egui, which draws UTF-8
            # correctly from a font atlas and never touches a code page.
            if "/ui/" in rel:
                continue
            src = path.read_text(encoding="utf-8")
            # Cheap pre-filter: the literal parser is char-by-char Python, and
            # the overwhelming majority of files contain none of these
            # characters anywhere. Skipping them keeps this test in the tens
            # of milliseconds rather than tens of seconds.
            if not any(c in src for c in TYPOGRAPHIC):
                continue
            if _is_test_source(rel, src):
                continue
            # Inline `#[cfg(test)] mod tests` is conventionally last in the
            # file; everything from there down is test-only.
            inline_tests = re.search(r"^#\[cfg\(test\)\]", src, re.M)
            if inline_tests:
                src = src[: inline_tests.start()]
            for line, literal in _string_literals(src):
                for bad in sorted({c for c in literal if c in TYPOGRAPHIC}):
                    violations.append(
                        f"{rel}:{line}: {bad!r} (use {TYPOGRAPHIC[bad]!r}) in {' '.join(literal.split())[:70]}"
                    )
        self.assertEqual(
            violations,
            [],
            "Typographic punctuation in strings that can reach stdout/stderr; "
            "these garble under cmd.exe on a legacy code page (AGENTS.md). "
            "Replace with the ASCII equivalent shown:\n" + "\n".join(violations),
        )

    def test_scanner_finds_a_planted_violation(self):
        """The guard must actually detect what it claims to.

        Without this, a bug in the literal parser (raw strings, escapes,
        comment stripping) would make the test vacuously pass and the rule
        would silently stop being enforced -- the exact failure mode that let
        28 violations accumulate in the first place.
        """
        planted = 'fn f() { println!("a \\" b — c"); }'
        found = [
            c
            for _line, lit in _string_literals(planted)
            for c in lit
            if c in TYPOGRAPHIC
        ]
        self.assertEqual(found, ["—"], "scanner must see through escapes")

        # A raw string must be scanned, not skipped...
        raw = 'const R: &str = r#"x → y"#;'
        self.assertTrue(
            any(c in TYPOGRAPHIC for _l, lit in _string_literals(raw) for c in lit),
            "raw string contents must be scanned",
        )
        # ...and a comment must be skipped, not scanned.
        commented = "// a — dash in a comment\nlet x = 1;"
        self.assertEqual(
            [c for _l, lit in _string_literals(commented) for c in lit if c in TYPOGRAPHIC],
            [],
            "comments never reach a console and must not be flagged",
        )

    def test_allowlist_entries_still_exist_and_are_justified(self):
        """A stale allowlist silently widens the exemption."""
        for rel, reason in ALLOWLIST.items():
            self.assertTrue(
                Path(rel).is_file(),
                f"allowlisted file no longer exists, drop the entry: {rel}",
            )
            self.assertTrue(reason.strip(), f"allowlist entry needs a reason: {rel}")
