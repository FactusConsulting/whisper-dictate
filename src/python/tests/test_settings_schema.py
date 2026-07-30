"""Guards for the shared runtime-settings schema (single source of truth).

settings_schema.json is consumed by both the transitional Python worker
(vp_config.py) and the Rust controller (config/schema.rs via include_str!).
These tests fail loudly if a side stops deriving from the schema, if a second
tracked copy appears, or if packaging stops shipping the canonical file.
"""
import subprocess
from tempfile import TemporaryDirectory

from helpers import Path, json, unittest

from whisper_dictate import vp_config

SCHEMA_PATH = Path("shared/config/settings_schema.json")


class SettingsSchemaTests(unittest.TestCase):
    def _schema_rows(self):
        return json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))["settings"]

    def test_schema_file_is_valid_and_nonempty(self):
        rows = self._schema_rows()
        self.assertGreater(len(rows), 0)
        for row in rows:
            self.assertTrue(row["env"].startswith("VOICEPI_"), row["env"])
            self.assertIn("key", row)
            self.assertIn("live", row)

    def test_python_settings_are_built_from_schema(self):
        rows = self._schema_rows()
        self.assertEqual(vp_config.settings_schema_path(), SCHEMA_PATH.resolve())
        self.assertEqual(len(vp_config.SETTINGS), len(rows))
        by_key = {s.key: s for s in vp_config.SETTINGS}
        for row in rows:
            setting = by_key[row["key"]]
            self.assertEqual(setting.env, row["env"], row["key"])
            self.assertEqual(setting.default, row.get("default"), row["key"])
            self.assertEqual(setting.live, bool(row.get("live", True)), row["key"])

    def test_setting_lookups_have_no_duplicates(self):
        envs = [s.env for s in vp_config.SETTINGS]
        keys = [s.key for s in vp_config.SETTINGS]
        self.assertEqual(len(vp_config.SETTING_BY_ENV), len(envs))
        self.assertEqual(len(vp_config.SETTING_BY_KEY), len(keys))
        self.assertEqual(len(set(envs)), len(envs))
        self.assertEqual(len(set(keys)), len(keys))

    def test_sentinel_defaults(self):
        by_key = {s.key: s for s in vp_config.SETTINGS}
        self.assertEqual(by_key["model"].default, "large-v3-turbo")
        self.assertEqual(by_key["stt_base_url"].default, "https://api.openai.com/v1")
        self.assertEqual(by_key["temperature"].default, "0.0,0.2")
        self.assertIsNone(by_key["lang"].default)

    def test_schema_resolver_supports_installed_app_layout(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            installed_schema = root / vp_config.SETTINGS_SCHEMA_RELATIVE_PATH
            installed_schema.parent.mkdir(parents=True)
            installed_schema.write_text('{"settings": []}', encoding="utf-8")
            module = root / "src/python/whisper_dictate/vp_config.py"
            module.parent.mkdir(parents=True)
            module.touch()
            self.assertEqual(
                vp_config.settings_schema_path(module),
                installed_schema.resolve(),
            )

    def test_exactly_one_canonical_schema_is_tracked(self):
        tracked_or_new = subprocess.check_output(
            [
                "git",
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "*settings_schema.json",
            ],
            text=True,
            encoding="utf-8",
        ).splitlines()
        present = [path for path in tracked_or_new if Path(path).is_file()]
        self.assertEqual(["shared/config/settings_schema.json"], present)

    def test_rust_controller_embeds_the_same_schema_file(self):
        config_rs = Path("src/rust/config/schema.rs").read_text(encoding="utf-8")
        self.assertIn(
            'include_str!("../../../shared/config/settings_schema.json")',
            config_rs,
        )

    def test_schema_is_bundled_by_installer_portable_zip_and_nix(self):
        inno = Path(
            "packaging/windows/inno/whisper-dictate.iss"
        ).read_text(encoding="utf-8")
        portable = Path("scripts/windows/build-installer.ps1").read_text(
            encoding="utf-8"
        )
        nix = Path("nix/package.nix").read_text(encoding="utf-8")
        self.assertIn(
            r'Source: "..\..\..\shared\config\settings_schema.json"',
            inno,
        )
        self.assertIn(
            r"shared\config\settings_schema.json",
            portable,
        )
        self.assertIn(
            "install -Dm644 shared/config/settings_schema.json",
            nix,
        )
        self.assertIn(
            '"$out/lib/whisper-dictate/shared/config/settings_schema.json"',
            nix,
        )

    def test_release_archives_bundle_the_canonical_schema(self):
        release = Path(".github/workflows/release.yml").read_text(encoding="utf-8")
        windows = Path(
            ".github/workflows/windows-installer-build.yml"
        ).read_text(encoding="utf-8")
        self.assertIn(
            'cp shared/config/settings_schema.json "$d/shared/config/"',
            release,
        )
        self.assertIn(
            r"Copy-Item shared\config\settings_schema.json "
            r'(Join-Path $bundle "shared\config")',
            windows,
        )

    def test_windows_release_gate_tracks_shared_config(self):
        windows = Path(
            ".github/workflows/windows-installer-build.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("            shared/config/ \\", windows)


if __name__ == "__main__":
    unittest.main()
