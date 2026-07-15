"""Guards for the shared runtime-settings schema (single source of truth).

settings_schema.json is consumed by both the Python worker (vp_config.py) and
the Rust controller (config.rs via include_str!). These tests fail loudly if a
side stops deriving from the schema, if the schema drifts from what the loaders
expect, or if packaging stops shipping the file to the Python runtime.
"""
from helpers import Path, json, unittest

from whisper_dictate import vp_config

SCHEMA_PATH = Path("src/python/whisper_dictate/settings_schema.json")


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

    def test_rust_controller_embeds_the_same_schema_file(self):
        # Single source of truth: Rust must read THIS file, not a hand copy.
        # The config module was split into config/*.rs; read config.rs if it
        # still exists, else the whole config/ directory. The include_str! path
        # is relative to the file that holds it, so config/schema.rs (one level
        # deeper than a flat config.rs) carries one extra `../`.
        src = Path("src/rust")
        single = src / "config.rs"
        if single.exists():
            config_rs = single.read_text(encoding="utf-8")
            schema_path = "../python/whisper_dictate/settings_schema.json"
        else:
            config_rs = "\n".join(
                p.read_text(encoding="utf-8")
                for p in sorted((src / "config").rglob("*.rs"))
            )
            schema_path = "../../python/whisper_dictate/settings_schema.json"
        self.assertIn(
            f'include_str!("{schema_path}")',
            config_rs,
        )

    def test_schema_is_bundled_by_installer_and_nix(self):
        # The Python worker reads the schema at import, so it must ship with the
        # package on every install path that uses a *.py-only file list.
        inno = Path(
            "packaging/windows/inno/whisper-dictate.iss"
        ).read_text(encoding="utf-8")
        nix = Path("nix/package.nix").read_text(encoding="utf-8")
        self.assertIn(
            r'Source: "..\..\..\src\python\whisper_dictate\*.json"',
            inno,
        )
        self.assertIn("settings_schema.json", nix)


if __name__ == "__main__":
    unittest.main()
