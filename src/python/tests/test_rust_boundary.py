from helpers import (
    _env,
    json,
    patch,
    subprocess,
    unittest,
)


class RustBoundaryTests(unittest.TestCase):
    def test_helper_path_uses_voicepi_rust_injector(self):
        from whisper_dictate import vp_rust

        with _env(VOICEPI_RUST_INJECTOR=None):
            self.assertIsNone(vp_rust.helper_path())
        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"):
            self.assertEqual(vp_rust.helper_path(), "whisper-dictate")

    def test_run_json_helper_returns_none_without_helper(self):
        from whisper_dictate import vp_rust

        with _env(VOICEPI_RUST_INJECTOR=None):
            with patch("whisper_dictate.vp_rust.subprocess.run") as run:
                self.assertIsNone(vp_rust.run_json_helper("privacy", {"text": "hej"}))

        run.assert_not_called()

    def test_run_json_helper_uses_utf8_json_contract(self):
        from whisper_dictate import vp_rust

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"],
            0,
            stdout=json.dumps({"ok": True, "text": "æøå"}),
            stderr="",
        )
        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"):
            with patch("whisper_dictate.vp_rust.subprocess.run", return_value=completed) as run:
                result = vp_rust.run_json_helper(
                    "redact-text",
                    {"text": "æøå"},
                    "--flag",
                    timeout=3.5,
                )

        self.assertEqual(result, {"ok": True, "text": "æøå"})
        self.assertEqual(run.call_args.args[0], ["whisper-dictate", "redact-text", "--flag"])
        self.assertEqual(json.loads(run.call_args.kwargs["input"]), {"text": "æøå"})
        self.assertEqual(run.call_args.kwargs["encoding"], "utf-8")
        self.assertEqual(run.call_args.kwargs["errors"], "replace")
        self.assertEqual(run.call_args.kwargs["timeout"], 3.5)
        self.assertFalse(run.call_args.kwargs["shell"])
