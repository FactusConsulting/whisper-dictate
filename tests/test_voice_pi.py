from tests.test_helpers import (
    Path,
    unittest,
)


class TestSuiteSplitSmokeTests(unittest.TestCase):
    def test_runtime_suite_has_been_split_into_feature_files(self):
        test_files = [path.name for path in Path("tests").glob("test_*.py")]
        self.assertIn("test_audio.py", test_files)
        self.assertIn("test_rust_ui_installer.py", test_files)
        self.assertIn("test_dictionary_benchmark_history.py", test_files)

    def test_runtime_is_real_package_module_without_root_shim(self):
        runtime = Path("src/python/whisper_dictate/runtime.py").read_text(encoding="utf-8")

        self.assertFalse(Path("voice_pi.py").exists())
        self.assertIn("def main() -> None:", runtime)
        self.assertIn('if __name__ == "__main__":\n    main()', runtime)

    def test_runtime_python_files_are_discoverable_after_package_split(self):
        root_modules = sorted(Path(".").glob("vp_*.py"))
        package_modules = sorted(Path("src/python/whisper_dictate").glob("*.py"))
        expected_modules = {
            "__init__.py",
            "runtime.py",
            "vp_audio.py",
            "vp_benchmark.py",
            "vp_cli.py",
            "vp_config.py",
            "vp_dictionary.py",
            "vp_dictionary_suggest.py",
            "vp_external_api.py",
            "vp_inject.py",
            "vp_keymap.py",
            "vp_parakeet.py",
            "vp_postprocess.py",
            "vp_transcribe.py",
        }

        self.assertEqual([], root_modules)
        self.assertEqual(expected_modules, {path.name for path in package_modules})
        self.assertTrue(all(path.is_file() for path in package_modules))
        self.assertEqual(len(package_modules), len(set(package_modules)))


if __name__ == "__main__":
    unittest.main()
