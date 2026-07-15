"""Live Groq integration smoke tests for STT and post-processing (#37).

These hit Groq's real OpenAI-compatible endpoints, so they only run when a
GROQ_API_KEY is present (a CI secret or a local env var) and are skipped
otherwise. They verify auth + URL + model wiring end-to-end, not exact output.

Run locally:  GROQ_API_KEY=gsk_... py -3.12 -m pytest src/python/tests/test_groq_integration.py -q
"""
from __future__ import annotations

import os
import unittest

from helpers import real_numpy

GROQ_API_KEY = os.environ.get("GROQ_API_KEY", "").strip()
GROQ_BASE_URL = "https://api.groq.com/openai/v1"
GROQ_STT_MODEL = "whisper-large-v3-turbo"
GROQ_POST_MODEL = "llama-3.3-70b-versatile"

_SKIP_REASON = "set GROQ_API_KEY to run the live Groq integration tests"


@unittest.skipUnless(GROQ_API_KEY, _SKIP_REASON)
class GroqIntegrationTests(unittest.TestCase):
    """End-to-end calls against Groq. Network + a valid key required."""

    def setUp(self):
        # Route the external adapters at Groq with the live key. No Rust helper,
        # so the pure-Python urllib path is exercised.
        self._old = {
            k: os.environ.get(k)
            for k in (
                "VOICEPI_STT_BACKEND", "VOICEPI_STT_BASE_URL", "VOICEPI_STT_API_KEY",
                "VOICEPI_STT_MODEL", "VOICEPI_STT_TIMEOUT_MS", "VOICEPI_RUST_INJECTOR",
            )
        }
        os.environ.pop("VOICEPI_RUST_INJECTOR", None)
        os.environ["VOICEPI_STT_BACKEND"] = "openai"
        os.environ["VOICEPI_STT_BASE_URL"] = GROQ_BASE_URL
        os.environ["VOICEPI_STT_API_KEY"] = GROQ_API_KEY
        os.environ["VOICEPI_STT_MODEL"] = GROQ_STT_MODEL
        os.environ["VOICEPI_STT_TIMEOUT_MS"] = "30000"

    def tearDown(self):
        for k, v in self._old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def test_groq_stt_transcribes_audio_without_error(self):
        np = real_numpy()
        from whisper_dictate import vp_external_api

        # 2 s of a 440 Hz tone @ 16 kHz int16 — enough audio for Groq to accept
        # the request; the transcript content is irrelevant to this smoke.
        sr = 16000
        t = np.arange(sr * 2, dtype=np.float32) / sr
        audio = (np.sin(2 * np.pi * 440 * t) * 8000).astype(np.int16)

        model = vp_external_api.ExternalTranscriptionModel(GROQ_STT_MODEL)
        segments, info = model.transcribe(audio, language="en")

        # Structural assertions: the call round-tripped and returned the
        # adapter's (segments, info) shape with string text.
        text = "".join(getattr(s, "text", "") for s in segments)
        self.assertIsInstance(text, str)

    def test_groq_post_processing_cleans_text(self):
        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="groq",
            mode="clean",
            model=GROQ_POST_MODEL,
            base_url=GROQ_BASE_URL,
            api_key=GROQ_API_KEY,
        )
        result = vp_postprocess.postprocess_text(
            "hej   verden   this is  a   test", settings)

        self.assertTrue(result.text.strip(), "post-processing returned empty text")


if __name__ == "__main__":
    unittest.main()
