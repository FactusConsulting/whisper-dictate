from tests.test_helpers import (
    Path,
    unittest,
)


class TestSuiteSplitSmokeTests(unittest.TestCase):
    def test_voice_pi_suite_has_been_split_into_feature_files(self):
        test_files = [path.name for path in Path("tests").glob("test_*.py")]
        self.assertIn("test_audio.py", test_files)
        self.assertIn("test_rust_ui_installer.py", test_files)
        self.assertIn("test_dictionary_benchmark_history.py", test_files)


if __name__ == "__main__":
    unittest.main()
