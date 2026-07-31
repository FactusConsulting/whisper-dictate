"""Regression tests for the native PowerShell version bump helper."""
import pathlib
import shutil
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "scripts" / "dev" / "bump-version.ps1"
VERSION_FILES = (
    pathlib.Path("VERSION"),
    pathlib.Path("src/rust/Cargo.toml"),
    pathlib.Path("src/rust/Cargo.lock"),
    pathlib.Path("nix/package.nix"),
)


def _copy_version_inputs(root: pathlib.Path) -> None:
    for relative in VERSION_FILES:
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPO_ROOT / relative, destination)


class BumpVersionScriptTests(unittest.TestCase):
    def test_bump_preserves_crlf_cargo_lock(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            _copy_version_inputs(root)
            lock = root / "src/rust/Cargo.lock"
            lock.write_bytes(lock.read_bytes().replace(b"\r\n", b"\n").replace(b"\n", b"\r\n"))

            result = subprocess.run(
                [
                    "pwsh",
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    str(SCRIPT),
                    "-Version",
                    "1.22.7",
                    "-Root",
                    str(root),
                ],
                capture_output=True,
                text=True,
                encoding="utf-8",
                cwd=str(REPO_ROOT),
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            updated = lock.read_bytes()
            self.assertIn(b"\r\n", updated)
            self.assertIn(b'name = "whisper-dictate-app"\r\nversion = "1.22.7"', updated)
            for relative in VERSION_FILES:
                self.assertIn("1.22.7", (root / relative).read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
