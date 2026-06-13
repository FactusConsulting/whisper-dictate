"""Drift guard: docs/CONFIGURATION.md generated block must match the schema.

scripts/dev/gen_settings_docs.py renders the settings reference between the
markers in docs/CONFIGURATION.md from settings_schema.json (the single source of
truth). If someone edits the schema (adds a setting, changes a default or
description) without regenerating the docs, the committed block drifts. This
test fails loudly in that case.

Regenerate with:

    py -3.12 scripts/dev/gen_settings_docs.py

The test both (1) compares an in-process render to the committed block, and
(2) runs the script's own ``--check`` mode as a subprocess so the exit-code
contract is exercised the same way CI/a human would use it.
"""
import importlib.util
import pathlib
import subprocess
import sys
import unittest

# Location-independent: the suite must work regardless of pytest's CWD.
REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "scripts" / "dev" / "gen_settings_docs.py"
DOCS = REPO_ROOT / "docs" / "CONFIGURATION.md"


def _load_generator():
    spec = importlib.util.spec_from_file_location("gen_settings_docs", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SettingsDocsGeneratedTests(unittest.TestCase):
    def test_committed_block_matches_schema(self):
        gen = _load_generator()
        settings = gen.load_settings()
        block = gen.render_block(settings)
        doc = DOCS.read_text(encoding="utf-8")
        expected = gen.splice(doc, block)
        self.assertEqual(
            doc,
            expected,
            "docs/CONFIGURATION.md is out of sync with settings_schema.json. "
            "Regenerate with: py -3.12 scripts/dev/gen_settings_docs.py",
        )

    def test_markers_present_and_ordered(self):
        gen = _load_generator()
        doc = DOCS.read_text(encoding="utf-8")
        self.assertIn(gen.BEGIN_MARKER, doc)
        self.assertIn(gen.END_MARKER, doc)
        self.assertLess(
            doc.index(gen.BEGIN_MARKER),
            doc.index(gen.END_MARKER),
            "BEGIN marker must precede END marker",
        )

    def test_check_mode_exit_zero(self):
        # The committed docs are in sync, so --check must succeed.
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--check"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            cwd=str(REPO_ROOT),
        )
        self.assertEqual(
            result.returncode,
            0,
            f"gen_settings_docs.py --check failed:\n{result.stdout}\n{result.stderr}",
        )

    def test_every_schema_description_appears_in_docs(self):
        # The generator must reuse the schema's per-key description verbatim, so
        # docs prose for a key and the schema description stay one source.
        gen = _load_generator()
        doc = DOCS.read_text(encoding="utf-8")
        for setting in gen.load_settings():
            desc = setting.get("description", "")
            self.assertTrue(desc, f"setting {setting['key']} has no description")
            self.assertIn(
                gen._escape_cell(desc),
                doc,
                f"description for {setting['key']} not found in generated docs",
            )

    def test_bad_input_exits_2_distinct_from_check_drift_exit_1(self):
        # Exit-code contract (review flagged bad input was exiting 1, the same as
        # --check drift): unknown category -> 2, bad/missing markers -> 2.
        gen = _load_generator()
        settings = gen.load_settings()
        bad = [dict(settings[0], category="does-not-exist")]
        with self.assertRaises(SystemExit) as ctx:
            gen.render_block(bad)
        self.assertEqual(ctx.exception.code, 2)

        with self.assertRaises(SystemExit) as ctx:
            gen.splice("no markers here", "block")
        self.assertEqual(ctx.exception.code, 2)

        with self.assertRaises(SystemExit) as ctx:
            gen.splice(f"{gen.END_MARKER}\n{gen.BEGIN_MARKER}", "block")
        self.assertEqual(ctx.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
