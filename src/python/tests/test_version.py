"""Unit tests for runtime version resolution (`_version_from_files`).

`get_version` used to read only `<package>/VERSION` (which never exists), so the
installed worker reported `unknown`. It now walks up to the repo/bundle-root
VERSION file; these tests cover that lookup.
"""
from helpers import (
    Path,
    tempfile,
    unittest,
)

from whisper_dictate import runtime


class VersionFromFilesTests(unittest.TestCase):
    def test_finds_version_in_ancestor_dir(self):
        # Mirrors the real layout: VERSION at the root, package a few levels down.
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "VERSION").write_text("1.2.3\n", encoding="utf-8")
            pkg = root / "src" / "python" / "whisper_dictate"
            pkg.mkdir(parents=True)
            self.assertEqual(runtime._version_from_files(pkg), "1.2.3")

    def test_strips_leading_v(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "VERSION").write_text("v2.0.0", encoding="utf-8")
            self.assertEqual(runtime._version_from_files(root), "2.0.0")

    def test_returns_none_when_no_version_file(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertIsNone(runtime._version_from_files(Path(d)))

    def test_nearest_ancestor_version_wins(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "VERSION").write_text("1.0.0", encoding="utf-8")
            (root / "a").mkdir()
            (root / "a" / "VERSION").write_text("2.0.0", encoding="utf-8")
            sub = root / "a" / "b"
            sub.mkdir()
            self.assertEqual(runtime._version_from_files(sub), "2.0.0")

    def test_ignores_blank_version_file(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "VERSION").write_text("   \n", encoding="utf-8")
            self.assertIsNone(runtime._version_from_files(Path(d)))


if __name__ == "__main__":
    unittest.main()
