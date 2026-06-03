from tests.test_helpers import (
    Path,
    unittest,
)


class RustUiInstallerSuiteSplitSmokeTests(unittest.TestCase):
    def test_rust_ui_installer_suite_has_been_split(self):
        test_files = {path.name for path in Path("tests").glob("test_*.py")}
        self.assertIn("test_windows_installer_ui.py", test_files)
        self.assertIn("test_linux_installer_ui.py", test_files)
        self.assertIn("test_release_workflows.py", test_files)


if __name__ == "__main__":
    unittest.main()
