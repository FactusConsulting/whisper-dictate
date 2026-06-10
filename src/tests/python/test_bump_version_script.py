"""Tests for scripts/dev/bump_version.py — the four-file version bump helper.

Runs against a temp fixture tree (never the real repo files) and against the
real repo in --check mode (the four real files must always agree).
"""
import pathlib
import subprocess
import sys
import tempfile
import unittest

# Location-independent: derive the repo root from this file's position so the
# suite works no matter what CWD pytest is invoked from (IDEs vary).
REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "scripts" / "dev" / "bump_version.py"


def _make_tree(root: pathlib.Path, version: str) -> None:
    (root / "src" / "rust").mkdir(parents=True)
    (root / "nix").mkdir()
    (root / "VERSION").write_text(version + "\n", encoding="utf-8")
    (root / "src" / "rust" / "Cargo.toml").write_text(
        f'[package]\nname = "whisper-dictate-app"\nversion = "{version}"\n',
        encoding="utf-8")
    (root / "src" / "rust" / "Cargo.lock").write_text(
        '[[package]]\nname = "other"\nversion = "9.9.9"\n\n'
        f'[[package]]\nname = "whisper-dictate-app"\nversion = "{version}"\n',
        encoding="utf-8")
    (root / "nix" / "package.nix").write_text(
        f'{{ version ? "{version}" }}: {{}}\n', encoding="utf-8")


def _run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True, text=True, encoding="utf-8", cwd=str(REPO_ROOT))


class BumpVersionScriptTests(unittest.TestCase):
    def test_bump_updates_all_four_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            _make_tree(root, "1.8.5")
            result = _run("1.8.6", "--root", str(root))

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((root / "VERSION").read_text(encoding="utf-8"),
                             "1.8.6\n")
            self.assertIn('version = "1.8.6"',
                          (root / "src/rust/Cargo.toml").read_text(encoding="utf-8"))
            lock = (root / "src/rust/Cargo.lock").read_text(encoding="utf-8")
            self.assertIn('name = "whisper-dictate-app"\nversion = "1.8.6"', lock)
            # Other crates' versions are untouched.
            self.assertIn('name = "other"\nversion = "9.9.9"', lock)
            self.assertIn('version ? "1.8.6"',
                          (root / "nix/package.nix").read_text(encoding="utf-8"))

    def test_refuses_inconsistent_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            _make_tree(root, "1.8.5")
            (root / "VERSION").write_text("1.8.4\n", encoding="utf-8")
            result = _run("1.8.6", "--root", str(root))
            self.assertEqual(result.returncode, 1)
            self.assertIn("INCONSISTENT", result.stderr)

    def test_missing_file_reports_not_found_instead_of_crashing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            _make_tree(root, "1.8.5")
            (root / "nix" / "package.nix").unlink()
            result = _run("--check", "--root", str(root))
            self.assertEqual(result.returncode, 1)
            self.assertIn("NOT FOUND", result.stdout)
            self.assertNotIn("Traceback", result.stderr)

    def test_pattern_drift_writes_nothing(self):
        # If one file's format drifted, NO file may be touched (no half-bump).
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            _make_tree(root, "1.8.5")
            # Single-quoted version: still "a version" to a human, but it no
            # longer matches the expected pattern — the bump must refuse and
            # leave every file untouched.
            (root / "nix" / "package.nix").write_text(
                "{ version ? '1.8.5' }: {}\n", encoding="utf-8")
            result = _run("1.8.6", "--root", str(root))
            self.assertEqual(result.returncode, 1)
            # Nothing was written anywhere.
            self.assertEqual((root / "VERSION").read_text(encoding="utf-8"),
                             "1.8.5\n")
            self.assertIn('version = "1.8.5"',
                          (root / "src/rust/Cargo.toml").read_text(encoding="utf-8"))

    def test_rejects_bad_version_string(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            _make_tree(root, "1.8.5")
            result = _run("v1.8.6", "--root", str(root))
            self.assertEqual(result.returncode, 1)

    def test_check_mode_passes_on_real_repo(self):
        # The actual repo's four version files must always agree.
        result = _run("--check")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
