"""Caller-side tests for the per-utterance ``[health]`` line.

The pure-logic units (``format_health_line`` / ``health_grade``) were ported to
Rust in PR #342 and are fully covered there:

  * ``src/rust/health/format.rs`` — confidence bands, mic segment, post segment,
    WARN flags, grade-segment placement.
  * ``src/rust/health/grade.rs`` — every priority branch of the 4-level grade.

The Python module remains as the caller-facing API used by the dictation loop
(switching Python to call the Rust helper is a follow-up). This file keeps:

  * A single smoke test per surface (``format_health_line`` / ``health_grade``)
    confirming the Python wrappers still produce the expected shape, so a
    regression in the Python module is caught even before the call-site moves
    to Rust.
  * ``ConfigDumpGatingTests`` — separate runtime concern (the startup config
    dump moved from Basic to Verbose diagnostics), not part of the Rust port.
"""
from __future__ import annotations

import os
import sys
import unittest
from contextlib import contextmanager
from pathlib import Path

sys.path.insert(0, str(Path("src/python")))

from whisper_dictate import runtime  # noqa: E402
from whisper_dictate.vp_health import (  # noqa: E402
    GRADE_PERFECT,
    format_health_line,
    health_grade,
)


@contextmanager
def _env(**values):
    saved = {k: os.environ.get(k) for k in values}
    try:
        for k, v in values.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        yield
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


class ConfigDumpGatingTests(unittest.TestCase):
    """The startup config dump moved from Basic to Verbose: it now requires
    BOTH VOICEPI_DEBUG and VOICEPI_STT_DEBUG (Verbose), not debug alone (Basic).
    """

    def test_dump_off_at_off_level(self):
        with _env(VOICEPI_DEBUG=None, VOICEPI_STT_DEBUG=None):
            self.assertFalse(runtime._config_dump_enabled())

    def test_dump_off_at_basic_level(self):
        # Basic = debug:on, stt_debug:off -> NO config dump (health line instead)
        with _env(VOICEPI_DEBUG="1", VOICEPI_STT_DEBUG=None):
            self.assertFalse(runtime._config_dump_enabled())

    def test_dump_on_at_verbose_level(self):
        # Verbose = debug:on, stt_debug:on -> config dump prints
        with _env(VOICEPI_DEBUG="1", VOICEPI_STT_DEBUG="1"):
            self.assertTrue(runtime._config_dump_enabled())


class PythonWrapperSmokeTests(unittest.TestCase):
    """One assertion per caller API so a regression in the Python wrappers is
    caught even before the call-site moves to the Rust helper. The exhaustive
    behaviour matrix is asserted in ``src/rust/health/{format,grade}.rs``.
    """

    METRICS = {
        "audio_raw_dbfs": -38.0,
        "audio_snr_db": 42.0,
        "audio_input_status": "good",
        "post_mode": "clean",
        "post_processor": "groq",
        "post_fallback": False,
        "segments": [{"avg_logprob": -0.10}],
    }

    def test_format_health_line_renders_expected_segments(self):
        line = format_health_line(self.METRICS)
        # One smoke assertion covering all four segments (mic / confidence / post
        # / trailing grade=) — full per-segment coverage lives in Rust.
        self.assertEqual(
            line,
            "[health] mic -38dBFS SNR 42dB good | confidence high (-0.10)"
            " | post clean/groq | grade=perfect",
        )

    def test_health_grade_returns_perfect_for_pristine_input(self):
        self.assertEqual(health_grade(self.METRICS), GRADE_PERFECT)


if __name__ == "__main__":
    unittest.main()
