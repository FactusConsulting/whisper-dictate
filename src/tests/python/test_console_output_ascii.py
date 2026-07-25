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

# Exemptions, scoped to a SPECIFIC literal rather than a whole file: keyed by
# path, each entry is (substring identifying the literal, reason). Exempting a
# whole file would silently cover every string added to it later.
ALLOWLIST = {
    "src/rust/postprocess/prompt.rs": [
        (
            "becomes|bliver til",
            "regex that MATCHES a user typing an arrow in a dictionary prompt "
            "-- input parsing, not console output; dropping it would silently "
            "stop recognising prompts people already write",
        ),
    ],
}

# Only a TRAILING `#[cfg(test)] mod ...` marks the rest of a file test-only.
#
# A bare `#[cfg(test)]` on a single mid-file item must NOT truncate the scan.
# `hotkey/manager/evdev_driver.rs` has exactly that shape -- a test-only
# `spawn` helper at line 91 with production code, including `eprintln!`
# console output, resuming below it. Truncating at the first attribute made
# this guard skip those lines entirely, which review caught: the check was
# passing on two real violations it was written to find.
TRAILING_TEST_MOD = re.compile(r"^#\[cfg\(test\)\]\s*\n\s*(?:pub\s+)?mod\s+\w+", re.M)

# `'x'`, `'\n'`, `'\''`, `'"'`, `'\u{263a}'` -- but NOT the lifetime `'static`.
CHAR_LITERAL = re.compile(r"'(?:\\u\{[^}]*\}|\\.|[^'\\])'")

# `r"`, `r#"`, `r##"`, and the byte-prefixed `br"` / `br#"` forms.
RAW_STRING_OPEN = re.compile(r'b?r(?P<hashes>#*)"')


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
        # Character literals must be consumed BEFORE the `"` branch below.
        # `let q = '"';` is real code here (keymap.rs, runtime/mod.rs), and
        # treating its embedded quote as a string opener desynchronises the
        # parser for the rest of the file -- every later console string would
        # be silently skipped. A lifetime (`'static`) is not a char literal,
        # so it falls through and is stepped over one char at a time.
        if ch == "'":
            char_lit = CHAR_LITERAL.match(src, i)
            if char_lit:
                i = char_lit.end()
                continue
            i += 1
            continue
        # Raw strings, with an optional `b` byte prefix. The hash count is
        # captured because `r##"..."##` only ends at a quote followed by the
        # SAME number of hashes -- `r##"has "# inside"##` is one literal, and
        # matching the first `"#` would truncate it and desync everything
        # after. The `b` prefix matters too: `br"..."` is 5 occurrences here,
        # and without it the parser fell through to the escaped-string branch
        # and mis-handled backslashes.
        raw = RAW_STRING_OPEN.match(src, i)
        if raw and not (i > 0 and (src[i - 1].isalnum() or src[i - 1] == "_")):
            closing = '"' + raw.group("hashes")
            end = src.find(closing, raw.end())
            end = n if end < 0 else end + len(closing)
            literal = src[i:end]
            yield line, literal
            line += literal.count("\n")
            i = end
            continue
        if ch == "b" and i + 1 < n and src[i + 1] == '"':
            # Byte string `b"..."`: same escape rules as a normal string, so
            # fall into the branch below with the quote as the start.
            i += 1
            ch = '"'
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


def scan_source(src: str, rel: str = "<memory>", exempt=()):
    """Return violation strings for one Rust source.

    Shared by the repo sweep and the generator test on purpose: a generator
    that exercised a COPY of this logic would prove nothing about the code
    that actually runs.
    """
    found = []
    trailing_tests = TRAILING_TEST_MOD.search(src)
    if trailing_tests:
        src = src[: trailing_tests.start()]
    for line, literal in _string_literals(src):
        if any(needle in literal for needle in exempt):
            continue
        for bad in sorted({c for c in literal if c in TYPOGRAPHIC}):
            found.append(
                f"{rel}:{line}: {bad!r} (use {TYPOGRAPHIC[bad]!r}) "
                f"in {' '.join(literal.split())[:70]}"
            )
    return found


class ConsoleOutputAsciiTests(unittest.TestCase):
    def test_no_typographic_punctuation_in_console_strings(self):
        violations = []
        for path in sorted(Path("src/rust").rglob("*.rs")):
            rel = path.as_posix()
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
            exempt = [needle for needle, _reason in ALLOWLIST.get(rel, [])]
            violations.extend(scan_source(src, rel, exempt))
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

    def test_allowlist_entries_still_match_a_real_literal(self):
        """A stale allowlist silently widens the exemption.

        Each entry must still identify a literal that actually exists AND
        still contains blocked punctuation -- otherwise the exemption has
        outlived the string it was written for and should be deleted.
        """
        for rel, entries in ALLOWLIST.items():
            self.assertTrue(
                Path(rel).is_file(),
                f"allowlisted file no longer exists, drop the entry: {rel}",
            )
            src = Path(rel).read_text(encoding="utf-8")
            for needle, reason in entries:
                self.assertTrue(reason.strip(), f"allowlist entry needs a reason: {rel}")
                matched = [
                    lit
                    for _line, lit in _string_literals(src)
                    if needle in lit and any(c in lit for c in TYPOGRAPHIC)
                ]
                self.assertTrue(
                    matched,
                    f"allowlist entry {needle!r} in {rel} no longer matches a "
                    "literal with blocked punctuation -- delete the entry",
                )

    def test_a_bare_cfg_test_item_does_not_hide_later_production_code(self):
        """Regression: #576 review.

        A bare `#[cfg(test)]` on ONE mid-file item used to truncate the whole
        scan, so production console output below it was never checked. Real
        occurrence: `evdev_driver.rs` had a test-only `spawn` at line 91 and
        live `eprintln!` output below -- the guard passed on two genuine
        violations.
        """
        src = (
            "#[cfg(test)]\n"
            "fn helper() {}\n"
            'fn production() { eprintln!("a — b"); }\n'
            "#[cfg(test)]\n"
            "mod tests {\n"
            '    const X: &str = "in — tests";\n'
            "}\n"
        )
        trailing = TRAILING_TEST_MOD.search(src)
        self.assertIsNotNone(trailing, "trailing `mod tests` must still be found")
        scanned = src[: trailing.start()]
        self.assertIn("production", scanned, "production code must survive the cut")
        self.assertNotIn("in — tests", scanned, "the test module must still be cut")

    def test_char_literal_does_not_desync_the_parser(self):
        """Regression: #576 review.

        `let q = '"';` is real code here. Treating that embedded quote as a
        string opener consumed everything up to the next quote, silently
        skipping every console string after it in the file.
        """
        src = "fn f() {\n" "    let q = '\"';\n" '    println!("bad — output");\n' "}\n"
        found = [
            c for _line, lit in _string_literals(src) for c in lit if c in TYPOGRAPHIC
        ]
        self.assertEqual(
            found, ["—"], "a string after a `'\"'` char literal must still be scanned"
        )
        # A lifetime is not a char literal and must not swallow anything.
        lifetime = "struct S<'a> { s: &'a str }\n" 'fn g() { println!("x — y"); }\n'
        self.assertEqual(
            [
                c
                for _line, lit in _string_literals(lifetime)
                for c in lit
                if c in TYPOGRAPHIC
            ],
            ["—"],
            "lifetimes must not be mistaken for char literals",
        )

    # ------------------------------------------------------------------
    # Class-level guard on the parser itself.
    #
    # Every bug found in review of this PR lived in a construct I had not
    # thought to write a case for -- and the response of adding one more
    # hand-picked case per bug leaves the parser validated against exactly
    # the shapes I imagined, which is the pattern that just failed.
    #
    # So: synthesise sources from random COMBINATIONS of the constructs that
    # make Rust literal parsing hard, plant violations at known positions,
    # and require exact recovery. This explores shapes nobody enumerated.
    # Seeded and bounded, so it is deterministic -- a failure reproduces from
    # the seed printed in the assertion message.
    # ------------------------------------------------------------------

    def _synthesize(self, rng):
        """Build a random Rust-ish source; return (src, expected markers).

        Each violating literal carries a unique `Vnn` marker, so the
        assertion compares SETS of markers rather than counts -- that catches
        a scanner that finds the right number of violations in the wrong
        places.
        """
        lines, expected = [], set()
        counter = [0]

        def marker():
            counter[0] += 1
            return f"V{counter[0]:02d}"

        def dash():
            return rng.choice(list(TYPOGRAPHIC))

        emitters = []

        def plain(violating):
            m = marker()
            body = f"{m} plain{dash() if violating else ''} tail"
            if violating:
                expected.add(m)
            lines.append(f'    println!("{body}");')

        def with_escaped_quote(violating):
            m = marker()
            body = f'{m} say \\"hi\\"{dash() if violating else ""}'
            if violating:
                expected.add(m)
            lines.append(f'    println!("{body}");')

        def raw(violating):
            m = marker()
            hashes = "#" * rng.randint(0, 2)
            # A quote-plus-fewer-hashes sequence INSIDE the literal must not
            # end it -- the classic raw-string truncation bug.
            inner = '"' + "#" * max(0, len(hashes) - 1) + " embedded" if hashes else ""
            body = f"{m} raw{inner}{dash() if violating else ''}"
            if violating:
                expected.add(m)
            prefix = rng.choice(["r", "br"])
            lines.append(f'    let _ = {prefix}{hashes}"{body}"{hashes};')

        def byte_string(violating):
            m = marker()
            # Byte strings cannot hold non-ASCII, so a "violating" one plants
            # its dash in a NORMAL string on the same line -- the point is
            # that the byte string must not swallow it.
            lines.append(f'    let _ = b"{m} bytes\\\\";')
            if violating:
                expected.add(m + "x")
                lines.append(f'    println!("{m}x after{dash()}");')

        def char_literal(violating):
            lit = rng.choice(["'\"'", "'\\''", "'\\\\'", "'x'", "'\\n'"])
            lines.append(f"    let _c = {lit};")
            if violating:
                m = marker()
                expected.add(m)
                lines.append(f'    println!("{m} after char{dash()}");')

        def lifetime(violating):
            lines.append("    fn g<'a>(s: &'a str) -> &'a str { s }")
            if violating:
                m = marker()
                expected.add(m)
                lines.append(f'    println!("{m} after lifetime{dash()}");')

        def comment(violating):
            # Comments never reach a console: a dash in one is NEVER expected,
            # even when `violating` is true. Quotes inside must not open a
            # string, and `//` or `/*` inside a STRING must not open a comment.
            kind = rng.choice(["line", "block"])
            if kind == "line":
                lines.append(f'    // a {dash()} and a " quote in a comment')
            else:
                lines.append(f'    /* {dash()} and " here\n       and more */')
            if violating:
                m = marker()
                expected.add(m)
                lines.append(f'    println!("{m} url http://x /* not a comment {dash()}");')

        emitters = [plain, with_escaped_quote, raw, byte_string, char_literal, lifetime, comment]

        lines.append("fn production() {")
        for _ in range(rng.randint(3, 12)):
            rng.choice(emitters)(rng.random() < 0.5)
        lines.append("}")

        # A bare `#[cfg(test)]` on one item must NOT hide production code
        # below it (the evdev_driver.rs shape).
        if rng.random() < 0.5:
            lines.append("#[cfg(test)]")
            lines.append("fn only_for_tests() {}")
            m = marker()
            expected.add(m)
            lines.append(f'fn still_production() {{ eprintln!("{m} below{dash()}"); }}')

        # A trailing test module MUST be cut: its violations are not expected.
        if rng.random() < 0.5:
            lines.append("#[cfg(test)]")
            lines.append("mod tests {")
            lines.append(f'    const NEVER: &str = "NEVER reported{dash()}";')
            lines.append("}")

        return "\n".join(lines) + "\n", expected

    def test_generated_sources_are_scanned_exactly(self):
        import random

        for seed in range(300):
            rng = random.Random(seed)
            src, expected = self._synthesize(rng)
            reported = scan_source(src)
            found = {m for m in expected | {"NEVER"} if any(m in r for r in reported)}
            self.assertEqual(
                found,
                expected,
                f"seed={seed}\nexpected markers {sorted(expected)}\n"
                f"reported:\n" + "\n".join(reported) + f"\n--- source ---\n{src}",
            )
