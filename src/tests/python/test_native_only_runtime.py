"""Negative guards for the retired Python production runtime.

Python remains a valid repository-test and release-metadata tool. It must not
return to the installed application, archives, native launch graph, or UI.
"""

import re
from helpers import Path, unittest


ROOT = Path(".")


def read(path: str) -> str:
    return Path(path).read_text(encoding="utf-8")


def production_rust() -> str:
    paths = sorted(
        path
        for path in Path("src/rust").rglob("*.rs")
        if "tests" not in path.parts and not path.name.endswith("_tests.rs")
    )
    return "\n".join(path.read_text(encoding="utf-8") for path in paths)


class NativeOnlyRuntimeTests(unittest.TestCase):
    def test_retired_python_payload_files_are_absent(self):
        for retired in (Path("src/python"), Path("requirements")):
            payload_files = [
                path
                for path in retired.rglob("*")
                if path.is_file()
                and not {"__pycache__", ".pytest_cache"}.intersection(path.parts)
                and path.suffix != ".pyc"
            ]
            self.assertEqual(
                payload_files,
                [],
                f"retired product payload exists under {retired.as_posix()}",
            )

    def test_native_code_has_no_python_process_launch(self):
        source = production_rust()
        forbidden_launches = (
            r'Command::new\(\s*"python(?:3|\.exe)?"',
            r"\bwhisper_dictate\.runtime\b",
            r"\bVOICEPI_PYTHON\b",
            r"\bVOICEPI_RUST_INJECTOR\b",
            r"\bPYTHONPATH\b",
        )
        for pattern in forbidden_launches:
            self.assertNotRegex(
                source,
                pattern,
                f"native production code references retired runtime marker {pattern!r}",
            )

    def test_cli_and_ui_expose_no_runtime_install_path(self):
        cli = read("src/rust/cli.rs")
        main = read("src/rust/main.rs")
        ui = "\n".join(
            path.read_text(encoding="utf-8")
            for path in sorted(Path("src/rust/ui").rglob("*.rs"))
            if not path.name.endswith("_tests.rs")
        )
        for marker in ("InstallRepair", "run_install", "run_install_command"):
            self.assertNotIn(marker, cli + main + ui)
        self.assertNotIn("Command::Install", main)
        self.assertNotRegex(cli, r"(?m)^\s*Install\s*(?:\{|,)")

    def test_active_docs_do_not_invoke_retired_install_command(self):
        docs = "\n".join(
            path.read_text(encoding="utf-8")
            for path in sorted(Path("docs").rglob("*.md"))
            if "archive" not in path.parts
        )
        self.assertNotRegex(
            docs,
            r"(?m)^(?:\./)?whisper-dictate(?:\.exe)?\s+install\b",
        )

    def test_windows_artifacts_package_native_payload_only(self):
        sources = {
            "Inno installer": read("packaging/windows/inno/whisper-dictate.iss"),
            "local portable zip": read("scripts/windows/build-installer.ps1"),
            "CI portable zip": read(".github/workflows/windows-installer-build.yml"),
        }
        forbidden = (
            "src\\python",
            "src/python",
            "requirements",
            "whisper_dictate.runtime",
            "VOICEPI_PYTHON",
            "PYTHONPATH",
        )
        for label, source in sources.items():
            for marker in forbidden:
                self.assertNotIn(marker, source, f"{label} still contains {marker!r}")

    def test_release_bundles_and_nix_have_no_product_python(self):
        release = read(".github/workflows/release.yml")
        nix = read("nix/flake.nix")

        for marker in (
            'cp -r src/python',
            'cp -r requirements',
            "whisper_dictate.runtime",
            "python -m venv",
            "pip install -r",
        ):
            self.assertNotIn(marker, release)
            self.assertNotIn(marker, nix)

        # Release validation explicitly names retired directories to prove they
        # are absent. Pin that negative check so deleting it cannot make CI pass
        # vacuously after an archive regression.
        self.assertIn("foreach ($retired in @('src\\python', 'requirements'))", release)
        self.assertIn("Retired payload was installed", release)

    def test_repository_python_is_test_tooling_not_product_runtime(self):
        workflow = read(".github/workflows/test.yml")
        self.assertIn("python -m pytest src/tests/python -q", workflow)
        self.assertNotIn("src/python/tests", workflow)
        self.assertNotIn("PYTHONPATH", workflow)
        self.assertNotIn("whisper_dictate.runtime", workflow)

    def test_automation_has_no_retired_runtime_callouts(self):
        roots = (
            Path("scripts"),
            Path("packaging"),
            Path("nix"),
            Path("docker"),
            Path(".devcontainer"),
        )
        forbidden = (
            "whisper_dictate.runtime",
            "whisper_dictate.vp_",
            "VOICEPI_PYTHON",
            "VOICEPI_RUST_INJECTOR",
            "PYTHONPATH",
            "src/python",
            "src\\python",
            "python -m whisper_dictate",
            "python3 -m whisper_dictate",
            "requirements/cpu.txt",
            "requirements/gpu.txt",
        )
        candidates = sorted(
            path
            for root in roots
            for path in root.rglob("*")
            if path.is_file()
            and "__pycache__" not in path.parts
            and path.suffix.lower() not in {".md", ".wav", ".png", ".pyc"}
        )
        violations = []
        for path in candidates:
            source = path.read_text(encoding="utf-8", errors="replace")
            for marker in forbidden:
                if marker in source:
                    violations.append(f"{path.as_posix()}: {marker}")
        self.assertEqual(
            violations,
            [],
            "automation still calls or configures the retired runtime:\n"
            + "\n".join(violations),
        )
