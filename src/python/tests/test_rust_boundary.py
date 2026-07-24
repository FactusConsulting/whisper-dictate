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


class NoConsoleWindowKwargsTests(unittest.TestCase):
    """Regression guard for the PR #564 two-binary split.

    Before the split, ``VOICEPI_RUST_INJECTOR`` pointed at the
    windows-subsystem GUI binary, so ``subprocess.run(...)`` invocations
    from the tray-launched Python worker never allocated a console. The
    split moved the env var to the console-subsystem CLI binary, which
    reintroduces a cmd-window flash on every helper call unless the
    caller passes ``creationflags=CREATE_NO_WINDOW`` on Windows.

    These tests pin the helper's platform-conditional contract AND assert
    that ``run_json_helper`` — the shared shell-out — actually applies it,
    so a regression that drops the kwarg from vp_rust.py fails here.
    """

    def test_returns_empty_dict_on_non_windows(self):
        from whisper_dictate import vp_rust

        with patch("whisper_dictate.vp_rust.os.name", "posix"):
            self.assertEqual(vp_rust.no_console_window_kwargs(), {})

    def test_returns_create_no_window_flag_on_windows(self):
        from whisper_dictate import vp_rust

        # 0x08000000 is the documented CREATE_NO_WINDOW flag; assert the
        # exact bit rather than trusting `subprocess.CREATE_NO_WINDOW` so
        # the test surfaces a regression if the constant ever gets
        # reassigned (very unlikely, but the value is load-bearing —
        # any other value silently re-enables the flash).
        with patch("whisper_dictate.vp_rust.os.name", "nt"):
            kwargs = vp_rust.no_console_window_kwargs()
        self.assertEqual(kwargs, {"creationflags": 0x08000000})

    def test_run_json_helper_forwards_the_kwargs_on_windows(self):
        from whisper_dictate import vp_rust

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 0, stdout="{}", stderr="",
        )
        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"):
            with patch("whisper_dictate.vp_rust.os.name", "nt"), \
                 patch("whisper_dictate.vp_rust.subprocess.run", return_value=completed) as run:
                vp_rust.run_json_helper("privacy", {})

        # The kwarg must land in the actual subprocess call — a regression
        # that adds the helper but forgets to plumb it through the run
        # site fails here even though the pure kwarg-shape test above
        # would still pass.
        self.assertEqual(run.call_args.kwargs.get("creationflags"), 0x08000000)

    def test_run_json_helper_omits_kwarg_on_non_windows(self):
        from whisper_dictate import vp_rust

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 0, stdout="{}", stderr="",
        )
        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"):
            with patch("whisper_dictate.vp_rust.os.name", "posix"), \
                 patch("whisper_dictate.vp_rust.subprocess.run", return_value=completed) as run:
                vp_rust.run_json_helper("privacy", {})

        # On POSIX the kwarg is meaningless (and Popen rejects unknown ones on
        # some Python builds) — the helper unpacks {} so the key is absent.
        self.assertNotIn("creationflags", run.call_args.kwargs)
