"""Live partial-transcription preview while recording (issue #16).

Today the whole buffer is transcribed only AFTER the push-to-talk key is
released; on long utterances (especially on CPU) the user waits with zero
feedback. This module runs a small background thread DURING recording that
periodically transcribes the accumulated buffer and emits a ``state="preview"``
worker event so the UI's live pipeline card can show the sentence growing.

It is strictly DISPLAY-ONLY. The preview:
  * never feeds back into the final transcription (the full-buffer pass at key
    release is unchanged — same quality),
  * never touches dictionary replacements, post-processing, injection, history
    or metrics,
  * swallows its own errors (logged at most once per recording) so a preview
    failure can never kill the worker.

CONCURRENCY — faster-whisper model calls are serialized by the module-level
``vp_transcribe.TRANSCRIBE_LOCK``. The preview acquires it ``blocking=False`` so
a tick is skipped whenever the final pass (or a previous preview) holds it —
previews therefore NEVER queue up. The cheap decode path
(``vp_transcribe.transcribe_preview``: beam_size=1, temperature 0.0,
condition_on_previous_text=False) is used so a preview is as fast as possible.

The preview only runs for the LOCAL "whisper" backend. It is NEVER used for the
"openai" cloud backend (that would spam a paid API).

numpy / vp_transcribe stay lazy: importing this module must not drag in the
heavy ML/audio stack. ``np`` / ``transcribe_preview`` / ``TRANSCRIBE_LOCK`` are
resolved from this module's namespace (which is also what the unit tests patch).
"""
from __future__ import annotations

import threading
import time

from whisper_dictate.vp_events import _compact_text, _emit_worker_event

# Populated lazily by _load_runtime_modules() (numpy + the transcribe lock and
# the cheap preview decode). Kept module-level so the tests can patch them.
np = None
TRANSCRIBE_LOCK = None
transcribe_preview = None

# A preview needs at least this much fresh audio since the previous preview
# before it is worth transcribing again — keeps short pauses between ticks from
# re-transcribing an essentially unchanged buffer.
MIN_NEW_AUDIO_S = 1.5

# Sliding-window cap (seconds): each preview decodes only the most recent
# PREVIEW_MAX_AUDIO_S seconds of audio rather than the entire buffer-so-far.
# Without this the live preview re-decodes the WHOLE accumulated buffer from
# t=0 on every tick — O(n) per tick, O(n^2) over the utterance, unbounded for
# long dictations (fine on GPU, quadratic/painful on CPU). Capping the decode
# input bounds each preview's cost to a constant.
#
# This intentionally makes the live preview show a ROLLING ~window of text (a
# live indicator of what you're saying NOW), NOT the full transcript-so-far.
# The FINAL transcription is a SEPARATE full-buffer decode (vp_dictate.py) that
# does NOT go through this module — it is completely unaffected by this cap.
PREVIEW_MAX_AUDIO_S = 15.0

# The preview text emitted to the UI is truncated to this many chars. Generous
# on purpose: the live card wraps over multiple lines so the user can read the
# whole sentence while still speaking (140 was far too tight for real
# dictation); the cap only guards the event payload against runaway essays.
PREVIEW_TEXT_CHARS = 600

# faster-whisper / CTranslate2 give "whisper"; the cloud backend does not run
# a local model we can cheaply re-decode mid-recording.
PREVIEW_BACKENDS = ("whisper",)


def _load_runtime_modules() -> None:
    """Populate the lazy numpy + transcribe globals used by PreviewEngine.

    Safe to call repeatedly. Kept here so PreviewEngine resolves ``np`` /
    ``transcribe_preview`` / ``TRANSCRIBE_LOCK`` from this module's namespace
    (which is also what the preview unit tests patch).
    """
    global np, TRANSCRIBE_LOCK, transcribe_preview

    import numpy as np  # noqa: F811
    from whisper_dictate.vp_transcribe import (  # noqa: F811
        TRANSCRIBE_LOCK, transcribe_preview,
    )


def preview_enabled(preview_seconds: float, stt_backend: str) -> bool:
    """True when the live preview should run for this session.

    Gated on a positive interval AND the LOCAL whisper backend. The cloud
    ("openai") backend is excluded so previews never hit a paid API.

    The Rust shell-out backend (``VOICEPI_TRANSCRIBE_BACKEND=rust``) is also
    excluded: it is a one-shot subprocess (writes a WAV, reloads the GGML
    model, runs inference, exits) with no streaming surface. Running previews
    against it would spawn a helper, reload the model, and burn CPU/disk every
    few seconds — and because each preview holds ``TRANSCRIBE_LOCK`` it would
    also delay the final pass on key release. Disable it cleanly here rather
    than wedge a cheap path into ``RustWhisperShellModel``; previews come
    back once the streaming Rust backend lands in Wave 7.
    """
    if preview_seconds <= 0:
        return False
    if (stt_backend or "").strip().lower() not in PREVIEW_BACKENDS:
        return False
    # Lazy import — vp_transcribe pulls in numpy/faster-whisper and we keep
    # this module light on import; the env-var check itself is cheap.
    try:
        from whisper_dictate.vp_transcribe import _rust_transcribe_enabled
    except ImportError:
        return True
    return not _rust_transcribe_enabled()


class PreviewEngine:
    """Background live-preview loop bound to a recording session.

    Reads the owner's live capture state (``owner.frames``, ``owner.recording``,
    ``owner.model``, ``owner.lang``) but writes nothing back to it — it only
    emits ``state="preview"`` worker events.
    """

    def __init__(self, owner, interval_s: float):
        self._owner = owner
        self._interval_s = max(0.0, float(interval_s))
        self._thread: threading.Thread | None = None
        self._stop = threading.Event()
        self._last_preview_samples = 0
        self._error_logged = False

    def start(self) -> None:
        if self._thread is not None or self._interval_s <= 0:
            return
        self._stop.clear()
        self._last_preview_samples = 0
        self._error_logged = False
        self._thread = threading.Thread(
            target=self._run, name="whisper-dictate-preview", daemon=True
        )
        self._thread.start()

    def stop(self) -> None:
        """Signal the loop to exit; it leaves promptly at the next wakeup."""
        self._stop.set()
        self._thread = None

    def _run(self) -> None:
        # Sleep on the stop Event (interruptible) instead of time.sleep so the
        # loop exits promptly when recording stops mid-interval.
        while not self._stop.wait(self._interval_s):
            if not self._owner.recording:
                break
            self._tick()

    def _snapshot_frames(self):
        """Recent-tail frames as one int16 array, plus total captured samples.

        Returns ``(pcm, total_samples)`` where ``pcm`` is the windowed decode
        input and ``total_samples`` is the length of the FULL accumulated buffer
        (before trimming). The frame list is snapshotted before concatenation;
        ``pcm`` may reuse storage for the single-frame fast path, so callers
        must treat it as read-only.

        Sliding window: ``pcm`` is trimmed to its last
        ``PREVIEW_MAX_AUDIO_S * SR`` samples so each preview decodes only the
        recent tail of audio (bounded cost) instead of the whole buffer-so-far.
        Buffers SHORTER than the window are returned in full (no change for
        short utterances). The FINAL transcription does NOT use this path and is
        unaffected — see the PREVIEW_MAX_AUDIO_S comment above.

        ``total_samples`` is returned UN-trimmed so the caller's fresh-audio gate
        and ``recording_s`` keep tracking real elapsed audio; if they used the
        capped length they'd freeze once the buffer exceeds the window and no
        further previews would ever fire.
        """
        frames = list(self._owner.frames)  # shallow copy of the list of chunks
        if not frames:
            return None
        from whisper_dictate.vp_capture import concat_capture_frames
        pcm = concat_capture_frames(frames)
        if pcm is None:
            return None
        total_samples = len(pcm)
        # Trim by SAMPLE COUNT on the concatenated int16 array. Slicing past the
        # start is safe (numpy clamps), so a short buffer is returned in full and
        # this never panics on an empty/short array. The window is in CAPTURE-rate
        # samples (native rate when a 16k open was rejected) so it always spans
        # PREVIEW_MAX_AUDIO_S of real audio regardless of the capture rate.
        max_samples = int(PREVIEW_MAX_AUDIO_S * self._capture_rate())
        if max_samples > 0 and total_samples > max_samples:
            pcm = pcm[-max_samples:]
        return pcm, total_samples

    def _capture_rate(self) -> int:
        """The owner's live capture sample rate (Hz); defaults to ``SR`` (16k).

        Native-rate capture (vp_capture) records at the device default rate and
        resamples to SR at consumption; the preview window / fresh-audio gate /
        recording_s are all in CAPTURED samples, so they must divide by this rate
        rather than SR to stay accurate when capture_rate != SR.
        """
        from whisper_dictate.vp_capture import SR
        return int(getattr(self._owner, "_capture_rate", SR) or SR)

    def _tick(self) -> None:
        """One preview attempt: gate on new audio, grab the lock, transcribe.

        All failures are swallowed (logged once) — a preview must never take
        down the worker.
        """
        try:
            snap = self._snapshot_frames()
            if snap is None:
                return
            # ``pcm`` is the windowed decode input (capped to PREVIEW_MAX_AUDIO_S);
            # ``samples`` is the TOTAL captured length, used for the fresh-audio
            # gate and recording_s so they keep advancing past the window.
            pcm, samples = snap
            # Skip when there isn't enough FRESH audio since the last preview;
            # avoids re-transcribing an essentially unchanged buffer. Gate is in
            # CAPTURED samples, so use the live capture rate (native or 16k).
            capture_rate = self._capture_rate()
            if samples - self._last_preview_samples < int(MIN_NEW_AUDIO_S * capture_rate):
                return
            # Non-blocking: if the final pass or a previous preview holds the
            # lock, drop this tick rather than queueing — previews must never
            # pile up behind the (higher-priority) final transcription.
            if not TRANSCRIBE_LOCK.acquire(blocking=False):
                return
            try:
                # Mono select like the final pass: collapse multi-channel to the
                # active channel so the cheap decode sees the same signal shape,
                # then resample the captured rate down to the model's 16k (no-op
                # for 16k-native devices). Same helper the final pass uses.
                from whisper_dictate.vp_capture import _resample_capture_buffer
                from whisper_dictate.vp_events import _select_active_channel_pcm
                mono = _select_active_channel_pcm(pcm).astype(np.int16)
                mono = _resample_capture_buffer(mono, capture_rate)
                text = transcribe_preview(self._owner.model, mono, self._owner.lang)
            finally:
                TRANSCRIBE_LOCK.release()
            self._last_preview_samples = samples
            if not text or not self._owner.recording:
                return
            _emit_worker_event(
                "status",
                state="preview",
                text_preview=_compact_text(text, PREVIEW_TEXT_CHARS),
                recording_s=round(samples / capture_rate, 2),
            )
        except Exception as e:  # noqa: BLE001 — preview must never kill the worker
            if not self._error_logged:
                print(f"[preview] failed: {e}", flush=True)
                self._error_logged = True
