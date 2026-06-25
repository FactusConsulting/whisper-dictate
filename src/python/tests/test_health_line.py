"""Unit tests for the per-utterance ``[health]`` line (Basic diagnostics)."""
from __future__ import annotations

import os
import sys
import unittest
from contextlib import contextmanager
from pathlib import Path

sys.path.insert(0, str(Path("src/python")))

from whisper_dictate import runtime  # noqa: E402
from whisper_dictate.vp_health import (  # noqa: E402
    GRADE_FAIR,
    GRADE_GOOD,
    GRADE_PERFECT,
    GRADE_POOR,
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


def _segments(*logprobs: float) -> list[dict]:
    return [{"avg_logprob": lp} for lp in logprobs]


class HealthLineConfidenceBandTests(unittest.TestCase):
    def _base(self, **over) -> dict:
        metrics = {
            "audio_raw_dbfs": -38.0,
            "audio_snr_db": 56.0,
            "audio_input_status": "good",
            "post_mode": "clean",
            "post_processor": "groq",
        }
        metrics.update(over)
        return metrics

    def test_high_band(self):
        line = format_health_line(self._base(segments=_segments(-0.13, -0.20)))
        self.assertIn("confidence high (-0.17)", line)
        self.assertNotIn("WARN", line)

    def test_ok_band(self):
        line = format_health_line(self._base(segments=_segments(-0.45, -0.50)))
        self.assertIn("confidence ok (-0.47)", line)
        self.assertNotIn("WARN", line)

    def test_low_band_warns(self):
        line = format_health_line(self._base(segments=_segments(-0.80, -0.90)))
        self.assertIn("confidence low (-0.85)", line)
        self.assertIn("WARN low confidence", line)

    def test_band_boundaries(self):
        # -0.35 is the high/ok boundary (>= -0.35 => high)
        self.assertIn("high", format_health_line(self._base(segments=_segments(-0.35))))
        self.assertIn("ok", format_health_line(self._base(segments=_segments(-0.36))))
        # -0.60 is the ok/low boundary (>= -0.60 => ok)
        self.assertIn("ok", format_health_line(self._base(segments=_segments(-0.60))))
        self.assertIn("low", format_health_line(self._base(segments=_segments(-0.61))))

    def test_missing_segments_is_na(self):
        line = format_health_line(self._base(segments=[]))
        self.assertIn("confidence n/a", line)
        self.assertNotIn("(", line.split("confidence", 1)[1].split("|", 1)[0])


class HealthLineMicTests(unittest.TestCase):
    def test_good_input(self):
        line = format_health_line({
            "audio_raw_dbfs": -38.2,
            "audio_snr_db": 56.4,
            "audio_input_status": "good",
            "segments": _segments(-0.1),
        })
        self.assertTrue(line.startswith("[health] mic -38dBFS SNR 56dB good |"))

    def test_quiet_with_boost(self):
        line = format_health_line({
            "audio_raw_dbfs": -44.0,
            "audio_snr_db": 40.0,
            "audio_input_status": "quiet",
            "audio_gain": 11.3,
            "segments": _segments(-0.2),
        })
        self.assertIn("quiet (boosted 11x)", line)
        # quiet + clean (high SNR) must NOT warn — that's the user's own setup.
        self.assertNotIn("WARN quiet input", line)

    def test_quiet_without_gain_has_no_boost_suffix(self):
        line = format_health_line({
            "audio_raw_dbfs": -44.0,
            "audio_snr_db": 40.0,
            "audio_input_status": "quiet",
            "segments": _segments(-0.2),
        })
        self.assertIn("mic -44dBFS SNR 40dB quiet", line)
        self.assertNotIn("boosted", line)


class HealthLineWarnTests(unittest.TestCase):
    def test_quiet_and_low_snr_warns(self):
        line = format_health_line({
            "audio_raw_dbfs": -55.0,
            "audio_snr_db": 3.0,
            "audio_input_status": "too_quiet",
            "segments": _segments(-0.2),
        })
        self.assertIn("WARN quiet input", line)

    def test_quiet_status_with_low_snr_number_warns(self):
        # status="quiet" (level low) AND a low SNR number -> "we're off".
        line = format_health_line({
            "audio_raw_dbfs": -50.0,
            "audio_snr_db": 4.0,
            "audio_input_status": "quiet",
            "segments": _segments(-0.2),
        })
        self.assertIn("WARN quiet input", line)

    def test_loud_but_low_snr_does_not_warn_quiet(self):
        # A loud-enough but noisy mic ("low_snr" status) is not "quiet input".
        line = format_health_line({
            "audio_raw_dbfs": -20.0,
            "audio_snr_db": 4.0,
            "audio_input_status": "low_snr",
            "segments": _segments(-0.2),
        })
        self.assertNotIn("WARN quiet input", line)

    def test_no_text_warns(self):
        line = format_health_line({"no_text": True})
        self.assertIn("WARN no text", line)

    def test_no_text_with_mic_metrics_renders_numbers(self):
        # When audio metrics are available at the no_text emit point the health
        # line must show the real mic level/SNR/status (not "mic ?dBFS SNR ?dB
        # n/a") so the user can diagnose why (was the input too quiet/noisy?).
        line = format_health_line({
            "no_text": True,
            "audio_raw_dbfs": -44.0,
            "audio_snr_db": 56.0,
            "audio_input_status": "quiet",
        })
        self.assertIn("mic -44dBFS", line)
        self.assertIn("SNR 56dB", line)
        self.assertIn("quiet", line)
        self.assertIn("WARN no text", line)
        self.assertNotIn("?dBFS", line)
        self.assertNotIn("SNR ?dB", line)


class HealthLinePostTests(unittest.TestCase):
    def test_post_on(self):
        line = format_health_line({
            "post_mode": "clean",
            "post_processor": "groq",
            "segments": _segments(-0.1),
        })
        self.assertIn("post clean/groq", line)

    def test_post_off_when_none(self):
        line = format_health_line({
            "post_mode": "raw",
            "post_processor": "none",
            "segments": _segments(-0.1),
        })
        self.assertIn("post off", line)

    def test_post_off_when_missing(self):
        line = format_health_line({"segments": _segments(-0.1)})
        self.assertIn("post off", line)

    def test_post_fallback_emits_warn_segment(self):
        line = format_health_line({
            "post_mode": "clean",
            "post_processor": "groq",
            "post_fallback": True,
            "post_latency_ms": 4012,
            "segments": _segments(-0.1),
        })
        # A WARN segment is appended so the Rust health card turns amber.
        self.assertIn("WARN post timeout->raw (4s)", line)
        # The WARN flags come after the stage text and before the trailing
        # grade= token (the grade is always the final segment now).
        segments = [seg.strip() for seg in line.split(" | ")]
        warn_index = segments.index("WARN post timeout->raw (4s)")
        self.assertTrue(segments[-1].startswith("grade="))
        self.assertLess(warn_index, len(segments) - 1)
        # The clean "post clean/groq" stage text is still present (provenance).
        self.assertIn("post clean/groq", line)

    def test_post_success_has_no_warn_segment(self):
        line = format_health_line({
            "post_mode": "clean",
            "post_processor": "groq",
            "post_fallback": False,
            "post_latency_ms": 800,
            "segments": _segments(-0.1),
        })
        self.assertIn("post clean/groq", line)
        self.assertNotIn("WARN", line)


class HealthGradeTests(unittest.TestCase):
    """The graduated 4-level quality grade (perfect/good/fair/poor)."""

    def test_perfect(self):
        # high confidence, good input, no fallback, pristine SNR.
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 42.0,
            "post_fallback": False,
            "segments": _segments(-0.10, -0.15),
        }
        self.assertEqual(health_grade(metrics), GRADE_PERFECT)

    def test_good(self):
        # high confidence and healthy SNR, but input merely "quiet" (boosted) —
        # not pristine enough for perfect, clearly above fair.
        metrics = {
            "audio_input_status": "quiet",
            "audio_snr_db": 24.0,
            "post_fallback": False,
            "segments": _segments(-0.20),
        }
        self.assertEqual(health_grade(metrics), GRADE_GOOD)

    def test_good_demoted_to_fair_when_post_fell_back(self):
        # Same clean signals, but post-processing timed out to raw -> fair.
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 42.0,
            "post_fallback": True,
            "segments": _segments(-0.10),
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_fair_on_ok_band(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 42.0,
            "segments": _segments(-0.45),  # "ok" band
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_fair_on_hot_input(self):
        metrics = {
            "audio_input_status": "hot",
            "audio_snr_db": 42.0,
            "segments": _segments(-0.10),
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_fair_when_high_band_but_mediocre_snr(self):
        # band "high" but SNR between POOR (6) and GOOD (20) -> only fair.
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 12.0,
            "segments": _segments(-0.10),
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_fair_when_audio_input_status_missing(self):
        # Codex P3 (PR #342): a missing audio_input_status is incomplete info,
        # so even with high confidence + clean SNR we must not claim "good".
        metrics = {
            "audio_snr_db": 42.0,
            "segments": _segments(-0.10),
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_fair_when_audio_input_status_empty_string(self):
        # Same as above but the key is present with an empty value — the
        # `str(value or "").strip()` coercion produces "" either way, so both
        # branches must agree the payload is incomplete.
        metrics = {
            "audio_input_status": "",
            "audio_snr_db": 42.0,
            "segments": _segments(-0.10),
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_good_for_openai_when_confidence_unavailable_but_audio_is_clean(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "stt_backend": "openai",
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "clean remote transcript"}],
        }
        self.assertEqual(health_grade(metrics), GRADE_GOOD)

        line = format_health_line(metrics)
        self.assertIn("confidence n/a", line)
        self.assertTrue(line.endswith("grade=good"))

    def test_openai_without_confidence_never_claims_perfect(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 56.0,
            "stt_backend": "openai",
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "clean remote transcript"}],
        }
        self.assertEqual(health_grade(metrics), GRADE_GOOD)

    def test_non_remote_missing_confidence_stays_fair(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "segments": [{"text": "local transcript without logprob"}],
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_explicit_non_openai_remote_missing_confidence_stays_fair(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "stt_backend": "custom",
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "remote transcript without logprob"}],
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_unknown_remote_missing_confidence_stays_fair(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "remote transcript without logprob"}],
        }
        self.assertEqual(health_grade(metrics), GRADE_FAIR)

    def test_poor_on_low_confidence(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 42.0,
            "segments": _segments(-0.80, -0.90),  # "low" band
        }
        self.assertEqual(health_grade(metrics), GRADE_POOR)

    def test_poor_on_bad_input_status(self):
        for status in ("too_quiet", "low_snr", "clip_risk"):
            metrics = {
                "audio_input_status": status,
                "audio_snr_db": 42.0,
                "segments": _segments(-0.10),
            }
            self.assertEqual(
                health_grade(metrics), GRADE_POOR, f"status={status}"
            )

    def test_poor_on_low_snr_number(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": 4.0,  # < HEALTH_SNR_POOR
            "segments": _segments(-0.10),
        }
        self.assertEqual(health_grade(metrics), GRADE_POOR)

    def test_missing_signals_default_to_fair_not_crash(self):
        # Empty dict: no segments (band n/a), no SNR, no status -> safe "fair".
        self.assertEqual(health_grade({}), GRADE_FAIR)
        # no_text-style dict (no segments) must not crash and must not over-claim.
        self.assertEqual(health_grade({"no_text": True}), GRADE_FAIR)

    def test_unparsable_snr_does_not_crash(self):
        metrics = {
            "audio_input_status": "good",
            "audio_snr_db": "n/a",
            "segments": _segments(-0.10),
        }
        # SNR unparsable -> treated as missing -> fair (never raises).
        self.assertEqual(health_grade(metrics), GRADE_FAIR)


class HealthLineGradeSegmentTests(unittest.TestCase):
    """`format_health_line` appends ` | grade=<g>` as its final segment."""

    def _line(self, **over) -> str:
        metrics = {
            "audio_raw_dbfs": -38.0,
            "audio_snr_db": 56.0,
            "audio_input_status": "good",
            "post_mode": "clean",
            "post_processor": "groq",
        }
        metrics.update(over)
        return format_health_line(metrics)

    def test_grade_is_last_segment(self):
        line = self._line(segments=_segments(-0.10))
        self.assertTrue(line.split(" | ")[-1].strip().startswith("grade="))

    def test_grade_matches_health_grade(self):
        # The token in the line must equal what health_grade returns for the
        # same metrics, across every level.
        cases = [
            dict(segments=_segments(-0.10), audio_snr_db=42.0),  # perfect
            dict(segments=_segments(-0.20), audio_snr_db=24.0,
                 audio_input_status="quiet"),  # good
            dict(segments=_segments(-0.45), audio_snr_db=42.0),  # fair
            dict(segments=_segments(-0.90), audio_snr_db=42.0),  # poor
        ]
        for over in cases:
            metrics = {
                "audio_raw_dbfs": -38.0,
                "audio_snr_db": 56.0,
                "audio_input_status": "good",
                "post_mode": "clean",
                "post_processor": "groq",
            }
            metrics.update(over)
            line = format_health_line(metrics)
            expected = health_grade(metrics)
            self.assertEqual(line.split(" | ")[-1].strip(), f"grade={expected}")

    def test_grade_segment_never_starts_with_warn(self):
        # The trailing grade segment must not collide with the structural
        # has_warning detection (a `| WARN ...` segment).
        line = self._line(segments=_segments(-0.90), audio_snr_db=42.0,
                           audio_input_status="too_quiet")
        last = line.split(" | ")[-1].strip()
        self.assertTrue(last.startswith("grade="))
        self.assertFalse(last.startswith("WARN"))


if __name__ == "__main__":
    unittest.main()
