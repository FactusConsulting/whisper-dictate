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
"openai" cloud backend (that would spam a paid API), and "parakeet" is skipped
for now.

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

# The preview text emitted to the UI is truncated to this many chars. Generous
# on purpose: the live card wraps over multiple lines so the user can read the
# whole sentence while still speaking (140 was far too tight for real
# dictation); the cap only guards the event payload against runaway essays.
PREVIEW_TEXT_CHARS = 600

# faster-whisper / CTranslate2 give "whisper"; the cloud + Parakeet backends do
# not run a local model we can cheaply re-decode mid-recording.
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
    ("openai") backend is excluded so previews never hit a paid API; Parakeet is
    skipped for now.
    """
    return preview_seconds > 0 and (stt_backend or "").strip().lower() in PREVIEW_BACKENDS


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
        """A COPY of the accumulated frames as one int16 array, or None.

        Copied (np.concatenate makes a new array) so the preview transcription
        never races the capture thread mutating ``owner.frames`` underneath it.
        """
        frames = list(self._owner.frames)  # shallow copy of the list of chunks
        if not frames:
            return None
        return np.concatenate(frames, axis=0).astype(np.int16)

    def _tick(self) -> None:
        """One preview attempt: gate on new audio, grab the lock, transcribe.

        All failures are swallowed (logged once) — a preview must never take
        down the worker.
        """
        try:
            pcm = self._snapshot_frames()
            if pcm is None:
                return
            samples = len(pcm)
            # Skip when there isn't enough FRESH audio since the last preview;
            # avoids re-transcribing an essentially unchanged buffer.
            from whisper_dictate.vp_capture import SR
            if samples - self._last_preview_samples < int(MIN_NEW_AUDIO_S * SR):
                return
            # Non-blocking: if the final pass or a previous preview holds the
            # lock, drop this tick rather than queueing — previews must never
            # pile up behind the (higher-priority) final transcription.
            if not TRANSCRIBE_LOCK.acquire(blocking=False):
                return
            try:
                # Mono select like the final pass: collapse multi-channel to the
                # active channel so the cheap decode sees the same signal shape.
                from whisper_dictate.vp_events import _select_active_channel_pcm
                mono = _select_active_channel_pcm(pcm).astype(np.int16)
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
                recording_s=round(samples / SR, 2),
            )
        except Exception as e:  # noqa: BLE001 — preview must never kill the worker
            if not self._error_logged:
                print(f"[preview] failed: {e}", flush=True)
                self._error_logged = True
