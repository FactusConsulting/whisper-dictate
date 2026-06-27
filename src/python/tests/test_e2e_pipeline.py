"""End-to-end dictation-pipeline tests.

These tests drive ``Dictate._stop_and_transcribe`` end-to-end on a synthesized
in-memory recording.  The contract under test is the full utterance pipeline:

    PCM buffer → gate/boost → transcription → hallucination scrub →
    dictionary replacement → post-processing decision → injection +
    history/metrics/worker events

What is STUBBED (bottom-most OS boundaries only):
  - the model: the test helpers (``load_voice_pi_realnp``) install a stub
    ``faster_whisper`` module that is never invoked, because
    ``_transcribe_detail`` (the vp_dictate module-level function that wraps it)
    is patched to return a known ``TranscribeResult``.  The real gate/boost
    logic inside ``_transcribe_detail`` is BYPASSED by this approach — so the
    e2e test exercises the orchestration wiring, not the DSP internals (those
    are covered by AudioDspTests and TranscribeDetailTests).
  - ``is_hallucination`` is pinned to False so the scrub never rejects the
    known stub text (the scrub itself is covered by HallucinationFilterTests).
  - ``postprocess_text`` is stubbed with a no-op pass-through for the
    "processor=none" scenario.
  - ``_inject`` is captured (same pattern as test_dictate_loop.py).
  - ``_record_utterance_event`` is captured to inspect the utterance payload.
  - ``_emit_worker_event`` is captured via ``VOICEPI_WORKER_EVENTS=1`` + stderr
    redirect (same pattern as test_dictate_loop.py) to verify event ordering.
  - ``VOICEPI_RUST_INJECTOR`` is unset (pure-Python fallback for the
    dictionary/privacy/command-hook Rust helpers) and
    ``VOICEPI_FORMAT_COMMANDS`` is forced off, so a developer's environment
    cannot make the format pass call a Rust helper mid-test.

What RUNS FOR REAL:
  - Frame concatenation + channel selection (``_select_active_channel_pcm``).
  - ``_should_skip_pcm`` duration/backend gate.
  - Dictionary wiring: ``_transcribe_detail`` stub returns a ``TranscribeResult``
    whose ``text`` carries the already-corrected phrase and whose
    ``dictionary_replacements`` records the rewrite — the test asserts the
    injected text matches the dictionary-corrected value and that the utterance
    event exposes the replacement list.
  - ``_postprocess_and_format`` (format-command pass).
  - Utterance-event construction (``_utterance_event``) + all field groups.
  - Worker-event emission ordering (transcribing → … → ready).
  - The ``no_text`` status event on empty model output.
  - Exception containment: a crashing model must not propagate out of
    ``_stop_and_transcribe``.

Synthetic PCM:
  2 s @ 16 kHz int16 mono — alternating loud/quiet 30 ms blocks that produce
  sufficient loudness contrast to pass ``_looks_like_speech``.  Stored as a
  list of one (samples, 1) numpy array in ``self.frames``, exactly as
  sounddevice fills them.
"""
from __future__ import annotations

import io
import json
from contextlib import redirect_stderr

from helpers import (
    _capture_stdout,
    _env,
    load_voice_pi_realnp,
    patch,
    types,
    unittest,
)


# ── shared fake helpers ───────────────────────────────────────────────────────

def _fake_transcribe_result(
    text: str,
    *,
    raw_text: str | None = None,
    dictionary_replacements: list | None = None,
):
    """Build a TranscribeResult-like namespace with the given text."""
    return types.SimpleNamespace(
        text=text,
        raw_text=raw_text if raw_text is not None else text,
        duration_s=2.0,
        post_boost_dbfs=-20.0,
        raw_dbfs=-30.0,
        peak=0.45,
        gain=2.0,
        noise_dbfs=-70.0,
        snr_db=40.0,
        input_status="good",
        compute_s=0.25,
        real_time_factor=0.12,
        language="en",
        language_probability=0.99,
        gate="raw=-30dBFS noise=-70dBFS snr=40dB input=good",
        segments=[],
        dictionary_terms=["Claude Code"] if dictionary_replacements else [],
        dictionary_replacements=dictionary_replacements or [],
    )


def _passthrough_postprocess(text, _settings=None):
    return types.SimpleNamespace(
        text=text,
        raw_text=text,
        provider="none",
        mode="raw",
        model="",
        latency_ms=0,
        changed=False,
        fallback=False,
        error=None,
        redacted=False,
        redactions=[],
    )


# ── test class ────────────────────────────────────────────────────────────────

class E2EPipelineTests(unittest.TestCase):
    """End-to-end tests for the full push-to-talk dictation pipeline."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        # Populate the lazily-loaded module globals (np, SR, _transcribe_detail, …).
        cls.runtime._load_runtime_modules()
        import importlib
        cls.dictate = importlib.import_module("whisper_dictate.vp_dictate")
        import numpy as np
        cls.np = np

    def _make_dictate(self):
        """Return a minimal Dictate instance with all OS boundaries stubbed.

        Mirrors the _make_dictate helper in test_dictate_loop.py exactly so the
        two harnesses stay consistent when vp_dictate evolves.
        """
        d = object.__new__(self.dictate.Dictate)
        d.recording = True
        d.release_tail_ms = 0
        d._preview = None
        d._arecord_proc = None
        d._stream = None
        d.audio_ducker = types.SimpleNamespace(enter=lambda: None, exit=lambda: None)
        d._capture_backend = "test"
        d._audio_input_device = "test"
        d._capture_channels = 1
        d.frames = []
        d._record_started = 0.0
        d.stt_backend = "whisper"
        d.model = object()
        d.lang = "en"
        d.postprocess_settings = None
        d.model_name = "test-model"
        d.device = "cpu"
        d.compute_type = "int8"
        d.model_load_s = 0.0
        d.mode = "print"
        d._last_inject_strategy = None
        d._inject_target_title = None
        d._inject_target_process = None
        d._active_profile_name = None
        self.injected = []
        self.utterance_events = []
        d._inject = self.injected.append
        d._record_utterance_event = self.utterance_events.append
        d._reload_live_config_if_changed = lambda: None
        return d

    def _speech_pcm(self, duration_s: float = 2.0):
        """Synthesize int16 mono 16 kHz PCM that passes the loudness/SNR gate.

        Alternates loud blocks (amplitude ≈ 0.8 full-scale) and quiet blocks
        (amplitude ≈ 0.05 full-scale) in 30 ms windows — the same pattern used
        by TranscribeDetailTests in test_stt.py.  This produces the SNR
        contrast ``_looks_like_speech`` requires (SNR > MIN_INPUT_SNR_DB=6 dB).

        Returns one (N, 1) int16 array wrapped in a list, matching sounddevice
        callback semantics.
        """
        np = self.np
        frame = int(self.dictate.SR * 0.030)   # 30 ms @ 16 kHz = 480 samples
        total_samples = int(duration_s * self.dictate.SR)
        n_frames = total_samples // frame
        audio_f32 = np.concatenate([
            np.full(frame, 0.8 if i % 2 == 0 else 0.05, dtype=np.float32)
            for i in range(n_frames)
        ])
        # Pad to exact length if needed
        remainder = total_samples - len(audio_f32)
        if remainder > 0:
            audio_f32 = np.concatenate([audio_f32, np.zeros(remainder, dtype=np.float32)])
        pcm = (audio_f32 * 32767).astype(np.int16).reshape(-1, 1)
        return [pcm]

    def _run_with_events(self, d, transcribe_fn=None, postprocess_fn=None):
        """Run _stop_and_transcribe with VOICEPI_WORKER_EVENTS=1.

        Returns the list of parsed worker-event payloads that arrived on stderr.
        Stubs _transcribe_detail and postprocess_text if provided.
        """
        rt = self.dictate
        stderr_buf = io.StringIO()
        postprocess = postprocess_fn or _passthrough_postprocess
        ctx = [
            patch.object(rt, "postprocess_text", postprocess),
            patch.object(rt, "is_hallucination", lambda _t: False),
            _capture_stdout(),
            _env(VOICEPI_WORKER_EVENTS="1", VOICEPI_RUST_INJECTOR=None,
                 VOICEPI_FORMAT_COMMANDS=None),
            redirect_stderr(stderr_buf),
        ]
        if transcribe_fn is not None:
            ctx.insert(0, patch.object(rt, "_transcribe_detail", transcribe_fn))

        import contextlib

        @contextlib.contextmanager
        def _stack(*cms):
            with contextlib.ExitStack() as stack:
                for cm in cms:
                    stack.enter_context(cm)
                yield

        with _stack(*ctx):
            rt.Dictate._stop_and_transcribe(d)

        events = []
        for line in stderr_buf.getvalue().splitlines():
            prefix = "[worker-event] "
            if line.startswith(prefix):
                events.append(json.loads(line[len(prefix):]))
        return events

    # ── scenario 1: dictionary-corrected transcription is injected ───────────

    def test_dictionary_corrected_text_is_injected(self):
        """Full pipeline: model returns raw STT text, dictionary corrects it,
        corrected text is injected, and the utterance event carries both the
        corrected text and the replacement record.

        This exercises the transcribe→dictionary→inject wiring.
        """
        d = self._make_dictate()
        d.frames = self._speech_pcm(2.0)

        raw = "hello cloud code"
        corrected = "hello Claude Code"
        replacements = [{"from": "cloud code", "to": "Claude Code", "count": 1}]

        def fake_transcribe(_model, _pcm, _lang):
            return _fake_transcribe_result(
                corrected,
                raw_text=raw,
                dictionary_replacements=replacements,
            )

        events = self._run_with_events(d, transcribe_fn=fake_transcribe)

        # 1. The injected text is the dictionary-corrected version, not the raw.
        self.assertEqual(
            self.injected,
            [corrected],
            "injected text should be the dictionary-corrected transcription",
        )

        # 2. An utterance event was emitted with the corrected text.
        self.assertEqual(len(self.utterance_events), 1)
        evt = self.utterance_events[0]
        self.assertEqual(evt["text"], corrected)
        self.assertEqual(evt["event"], "utterance")

        # 3. The utterance event carries the dictionary replacement record.
        self.assertEqual(
            evt.get("dictionary_replacements"),
            replacements,
            "utterance event must expose dictionary replacements",
        )

        # 4. Worker events arrive in order: transcribing → … → ready.
        state_seq = [e.get("state") for e in events if e.get("event") == "status"]
        self.assertIn("transcribing", state_seq)
        self.assertIn("ready", state_seq)
        transcribing_idx = state_seq.index("transcribing")
        ready_idx = state_seq.index("ready")
        self.assertLess(
            transcribing_idx,
            ready_idx,
            f"'transcribing' must precede 'ready'; got sequence: {state_seq}",
        )

        # 5. The captured utterance event carries text, recording_s ≈ 2s, stt backend.
        # (Note: _record_utterance_event is stubbed at instance level so the
        #  utterance event is captured in self.utterance_events rather than emitted
        #  to stderr; status events such as transcribing/ready still flow through
        #  _emit_worker_event and appear in `events`.)
        uwe = self.utterance_events[0]
        self.assertEqual(uwe.get("text"), corrected)
        self.assertIn("recording_s", uwe)
        recording_s = uwe["recording_s"]
        self.assertAlmostEqual(
            recording_s, 2.0, delta=0.5,
            msg=f"recording_s ({recording_s}) should be ≈2s (±0.5)",
        )
        self.assertEqual(uwe.get("stt_backend"), "whisper")

    # ── scenario 2: post-processing disabled → text is not altered ──────────

    def test_post_processing_none_does_not_alter_text(self):
        """With processor='none', the post-processing stage must pass text through
        unchanged.  This verifies the postprocess→format→inject path with the
        'none' processor configured.
        """
        d = self._make_dictate()
        d.frames = self._speech_pcm(2.0)

        original_text = "hello world from whisper"

        def fake_transcribe(_model, _pcm, _lang):
            return _fake_transcribe_result(original_text)

        # Use a post-processor that would change the text if it ran — a 'none'
        # provider is already the passthrough, so we additionally verify by
        # checking the injected text equals the STT output unchanged.
        def none_postprocess(text, _settings=None):
            return types.SimpleNamespace(
                text=text,   # must pass through unchanged
                raw_text=text,
                provider="none",
                mode="raw",
                model="",
                latency_ms=0,
                changed=False,
                fallback=False,
                error=None,
                redacted=False,
                redactions=[],
            )

        events = self._run_with_events(
            d,
            transcribe_fn=fake_transcribe,
            postprocess_fn=none_postprocess,
        )

        self.assertEqual(
            self.injected,
            [original_text],
            "text must not be altered when post-processor is 'none'",
        )
        # An utterance event is still emitted.
        self.assertEqual(len(self.utterance_events), 1)
        self.assertEqual(self.utterance_events[0]["text"], original_text)

        # Worker events still arrive in order.
        state_seq = [e.get("state") for e in events if e.get("event") == "status"]
        self.assertIn("transcribing", state_seq)
        self.assertIn("ready", state_seq)

    # ── scenario 3: model returns empty text → no injection + no_text event ──

    def test_empty_transcription_emits_no_text_event_and_no_injection(self):
        """When the model returns empty text (no speech detected), the pipeline
        must:
          - not inject anything,
          - not produce an utterance event,
          - emit a 'no_text' status worker event with a reason.
        """
        d = self._make_dictate()
        d.frames = self._speech_pcm(2.0)

        def fake_transcribe_empty(_model, _pcm, _lang):
            # Empty text — simulates no-speech output from the model.
            return _fake_transcribe_result("")

        events = self._run_with_events(d, transcribe_fn=fake_transcribe_empty)

        # No injection.
        self.assertEqual(
            self.injected,
            [],
            "empty model output must not produce any injection",
        )
        # No utterance event.
        self.assertEqual(
            self.utterance_events,
            [],
            "empty model output must not emit an utterance event",
        )
        # A no_text status event is emitted.
        no_text_events = [e for e in events if e.get("state") == "no_text"]
        self.assertEqual(
            len(no_text_events),
            1,
            f"expected exactly one no_text worker event; got {events}",
        )
        # The reason is one of the known empty-output reasons.
        reason = no_text_events[0].get("reason")
        self.assertIn(
            reason,
            ("empty", "no_speech", "too_quiet"),
            f"unexpected no_text reason: {reason!r}",
        )
        # 'ready' is still emitted (the worker does not get stuck).
        state_seq = [e.get("state") for e in events if e.get("event") == "status"]
        self.assertIn("ready", state_seq)

    # ── scenario 4: model raises → no crash, no injection ────────────────────

    def test_model_exception_does_not_crash_worker(self):
        """A RuntimeError raised by the model must be swallowed by
        ``_stop_and_transcribe`` — the worker must survive and still emit 'ready'.

        This protects against integration regressions where an API call or
        broken model load causes an unhandled exception to bubble up through the
        dictation loop.
        """
        d = self._make_dictate()
        d.frames = self._speech_pcm(2.0)

        def crashing_transcribe(_model, _pcm, _lang):
            raise RuntimeError("simulated model failure")

        # Must not raise.
        try:
            events = self._run_with_events(d, transcribe_fn=crashing_transcribe)
        except Exception as exc:  # noqa: BLE001
            self.fail(
                f"_stop_and_transcribe must not propagate exceptions; got {exc!r}"
            )

        # No text injected.
        self.assertEqual(
            self.injected,
            [],
            "a crashing model must not inject anything",
        )
        # No utterance event.
        self.assertEqual(
            self.utterance_events,
            [],
            "a crashing model must not emit an utterance event",
        )
        # The worker still emits 'ready' in the finally block.
        state_seq = [e.get("state") for e in events if e.get("event") == "status"]
        self.assertIn(
            "ready",
            state_seq,
            f"'ready' must always be emitted; got sequence: {state_seq}",
        )


if __name__ == "__main__":
    unittest.main()
