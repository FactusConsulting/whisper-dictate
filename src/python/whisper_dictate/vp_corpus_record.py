"""Record reference audio for a golden-corpus item FROM the app.

The benchmark scores pre-recorded *golden* audio. To make that corpus
self-service, this worker mode shows the reference text of a corpus item, the
user reads it aloud, and we record the clip straight to the per-user audio dir
(``<appdata>/benchmark/audio/<id>.wav``) where the benchmark already looks for
recordings (:func:`vp_benchmark_paths.resolve_item_audio`). No file juggling.

It REUSES the live-capture machinery end to end rather than reimplementing
device opening — the recording must work on every mic dictation works on (the
Blue Yeti included). A tiny :class:`_Recorder` mixes in
:class:`vp_capture.CaptureMixin` and drives the SAME negotiated open path:

  * :meth:`CaptureMixin._start_sounddevice` / :meth:`CaptureMixin._start_arecord`
    — full format negotiation, WASAPI auto_convert, the device-native-rate and
    the DirectSound/MME sibling-endpoint fallbacks,
  * :meth:`CaptureMixin._cb` / :meth:`CaptureMixin._arecord_reader` — the frame
    collection (already coercing float32→int16 via
    :func:`vp_capture._capture_frame_to_int16`),
  * :func:`vp_capture._resample_capture_buffer` — native-rate→16k resample reuse,
  * :meth:`CaptureMixin._stop_capture_streams` — clean teardown.

Like ``--test-audio-device``, this NEVER raises out: every failure (unknown id,
missing device, no corpus, sounddevice unavailable) is reported as a single
``corpus_record_error`` JSON line on stdout and the worker exits 0. The UI parses
the start/progress/done/error lines.
"""
from __future__ import annotations

import json
import threading
import time
import wave
from pathlib import Path

from whisper_dictate import vp_capture

# Speaking-pace heuristic: ~12 reference characters per spoken second (a relaxed,
# read-aloud cadence). Clamped so a one-liner still gets a usable window and a
# long paragraph never runs away. A fixed lead-in is added so the user has time
# to start speaking after the countdown begins.
_CHARS_PER_SECOND = 12.0
_MIN_RECORD_S = 8.0
_MAX_RECORD_S = 90.0
_LEAD_IN_S = 2.0
# Emit a countdown progress line roughly every this many seconds so the UI/log
# shows life during a long read instead of going silent.
_PROGRESS_EVERY_S = 5.0

# Allowlist for corpus IDs used as filename stems (<appdata>/benchmark/audio/<id>.wav).
# Must match the Rust counterpart ``is_safe_corpus_id`` in ``ui/corpus.rs``.
import re as _re
_SAFE_ID_RE = _re.compile(r'^[A-Za-z0-9._-]+$')


def is_safe_corpus_id(id_: str) -> bool:
    """Return True iff ``id_`` is safe to use as a filename stem.

    Allows only ``[A-Za-z0-9._-]``, rejects empty strings, ``.``, ``..``, and
    any value containing a path separator (``/`` or ``\\``).  Mirrors the Rust
    ``is_safe_corpus_id`` in ``ui/corpus.rs`` so the two runtimes apply the same
    rule.
    """
    if not id_ or id_ in (".", ".."):
        return False
    if "/" in id_ or "\\" in id_:
        return False
    return bool(_SAFE_ID_RE.match(id_))


def compute_record_seconds(text: str) -> float:
    """Recording length (s) for ``text``: chars/12 clamped to [8, 90] + 2s lead-in.

    Pure so the duration heuristic is unit-testable in isolation. The body length
    (chars/``_CHARS_PER_SECOND``) is clamped to ``[_MIN_RECORD_S, _MAX_RECORD_S]``
    BEFORE the fixed ``_LEAD_IN_S`` is added, so the clamp bounds the *speaking*
    window and every recording gets the same head start regardless of length.
    """
    chars = len((text or "").strip())
    body = chars / _CHARS_PER_SECOND
    body = max(_MIN_RECORD_S, min(_MAX_RECORD_S, body))
    return body + _LEAD_IN_S


def _print_event(payload: dict) -> None:
    """Print one JSON event line to stdout (the UI parses these), flushed.

    ``ensure_ascii=False`` so Danish reference text survives in the start line;
    the worker's stdout is UTF-8 (the launcher forces it on Windows).
    """
    print(json.dumps(payload, ensure_ascii=False), flush=True)


class _Recorder(vp_capture.CaptureMixin):
    """Minimal capture host: the SAME ``self.`` state ``Dictate`` sets up.

    Only the attributes the mixin's capture methods touch are initialised here
    (``frames`` / ``recording`` / the first-audio event / stream handles / the
    metering timestamps). No model, no PTT listener, no injection — this is the
    capture half of ``Dictate`` reused on its own for a fixed-duration record.
    """

    def __init__(self) -> None:
        self.frames: list = []
        self.recording = False
        self._record_started = 0.0
        self._first_audio_at = 0.0
        self._first_audio_event = threading.Event()
        self._last_audio_level_event = 0.0
        self._stream = None
        self._arecord_proc = None
        self._capture_backend = ""
        self._audio_input_device = ""
        self._capture_channels = 1
        self._capture_dtype = "int16"
        self._capture_rate = vp_capture.SR

    def _emit_audio_level(self, pcm) -> None:
        """Drop level metering: a one-shot record needs no live audio meter.

        The mixin's ``_cb`` / ``_arecord_reader`` call this per chunk to emit the
        UI meter event during live dictation. Here it would only spam stderr, so
        it is a no-op — frame collection (the part we DO reuse) is unaffected.
        """


def _start_capture(rec: _Recorder) -> None:
    """Open the configured mic via the negotiated capture path and start recording.

    Mirrors ``Dictate._start``'s backend choice: an available PipeWire arecord
    route uses :meth:`CaptureMixin._start_arecord`; otherwise
    :meth:`CaptureMixin._start_sounddevice` (the full WASAPI→DirectSound→MME
    open matrix). ``recording`` is set True FIRST so the callback/reader buffers
    frames from the moment the stream opens.
    """
    rec.recording = True
    rec._record_started = time.monotonic()
    if vp_capture._ensure_arecord_device():
        rec._start_arecord()
    else:
        rec._start_sounddevice()


def _collect_for(rec: _Recorder, seconds: float) -> None:
    """Sleep ``seconds`` while the capture thread buffers, emitting countdown lines.

    The actual frame collection runs on the sounddevice callback / arecord reader
    thread; this just waits out the window and prints a
    ``corpus_record_progress`` line about every ``_PROGRESS_EVERY_S`` seconds so a
    long read shows remaining time instead of going silent.
    """
    deadline = time.monotonic() + seconds
    next_progress = time.monotonic() + _PROGRESS_EVERY_S
    while True:
        now = time.monotonic()
        remaining = deadline - now
        if remaining <= 0:
            break
        if now >= next_progress:
            _print_event({
                "event": "corpus_record_progress",
                "remaining_s": int(round(remaining)),
            })
            next_progress = now + _PROGRESS_EVERY_S
        time.sleep(min(0.2, remaining))


def _frames_to_int16_mono(rec: _Recorder):
    """Concatenate the captured frames and resample native-rate→16k mono int16.

    Reuses :func:`vp_capture._resample_capture_buffer` (which itself reuses the
    ``vp_audio_file`` resample/convert helpers) so the recorded WAV matches the
    live-dictation 16k mono int16 contract — including the case where the device
    was opened at its native 44.1/48 kHz and the buffer must be downsampled.
    Returns an int16 numpy array shaped ``(N, 1)`` (empty ``(0, 1)`` if nothing
    was captured).
    """
    np = vp_capture.np
    if not rec.frames:
        return np.zeros((0, 1), dtype=np.int16)
    pcm = np.concatenate(rec.frames, axis=0)
    rate = getattr(rec, "_capture_rate", vp_capture.SR) or vp_capture.SR
    return vp_capture._resample_capture_buffer(pcm, rate)


def _write_wav(path: Path, pcm) -> None:
    """Write ``pcm`` (int16 mono) as a 16k mono 16-bit PCM WAV at ``path``.

    Creates parent dirs and overwrites any existing file — re-recording a corpus
    item is the whole point. Matches the benchmark's WAV contract (16-bit PCM,
    mono, 16 kHz) so the file is scored without an ffmpeg detour.
    """
    np = vp_capture.np
    path.parent.mkdir(parents=True, exist_ok=True)
    data = np.asarray(pcm, dtype=np.int16).reshape(-1)
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(int(vp_capture.SR))
        wav.writeframes(data.tobytes())


def _peak_rms_dbfs(pcm) -> tuple[float, float]:
    """Return ``(peak_dbfs, rms_dbfs)`` for an int16 buffer via the shared metrics.

    Reuses :func:`vp_events._audio_level_metrics` (the same RMS/peak math the live
    meter uses) so the recorded clip's level is reported on the same scale. A peak
    near 0 dBFS means clipping; a very low RMS means the user was too quiet — both
    actionable from the done event without replaying the clip.
    """
    import math

    from whisper_dictate.vp_events import _audio_level_metrics

    raw_dbfs, peak, _level = _audio_level_metrics(pcm)
    peak_dbfs = 20.0 * math.log10(peak) if peak > 0 else -120.0
    return round(peak_dbfs, 1), round(raw_dbfs, 1)


def _resolve_item(item_id: str, *, app_root, appdata):
    """Find the corpus item with ``item_id`` (corpus resolved like the benchmark).

    Reuses :func:`vp_benchmark.resolve_corpus_manifest` (app-root → appdata) and
    :func:`vp_benchmark.load_corpus` so the recorder sees exactly the same corpus
    the "Run benchmark" button does. Returns the matching ``CorpusItem`` or raises
    ``LookupError`` (no corpus / unknown id) — the caller turns that into a clean
    ``corpus_record_error`` event.
    """
    from whisper_dictate.vp_benchmark import load_corpus, resolve_corpus_manifest

    manifest = resolve_corpus_manifest(app_root, None, appdata)
    if manifest is None or not Path(manifest).exists():
        raise LookupError("no benchmark corpus found")
    for item in load_corpus(manifest):
        if item.id == item_id:
            return item
    raise LookupError(f"unknown corpus id: {item_id}")


def record_corpus_item(item_id: str, *, app_root=None, appdata=None) -> int:
    """``--record-corpus-item`` entry: record reference audio for ``item_id``.

    Resolves the corpus item, prints a ``corpus_record_start`` line (reference
    text + computed duration), records from the configured mic via the negotiated
    capture path for that duration (emitting ``corpus_record_progress`` lines),
    writes ``<appdata>/benchmark/audio/<id>.wav`` and prints ``corpus_record_done``
    with the saved path + level metrics — OR prints ``corpus_record_error`` on any
    failure. Always returns 0 (an error is a normal, reportable outcome, not a
    worker crash). ``item_id`` is stripped; ``app_root`` / ``appdata`` default to
    the live values when not injected (tests inject them).
    """
    from whisper_dictate.vp_benchmark_paths import appdata_audio_dir
    from whisper_dictate.vp_config import appdata_dir

    item_id = (item_id or "").strip()
    if not is_safe_corpus_id(item_id):
        _print_event({
            "event": "corpus_record_error",
            "error": f"unsafe corpus id: {item_id!r}",
        })
        return 0
    if appdata is None:
        appdata = appdata_dir()
    try:
        item = _resolve_item(item_id, app_root=app_root, appdata=appdata)
    except Exception as exc:  # noqa: BLE001 - report cleanly, never raise out
        _print_event({"event": "corpus_record_error", "error": str(exc)})
        return 0

    # numpy + SR must be materialised before any capture/resample (the live path
    # loads these lazily at startup; the utility subcommand never reached that).
    vp_capture._load_runtime_modules()

    seconds = compute_record_seconds(item.text)
    _print_event({
        "event": "corpus_record_start",
        "id": item.id,
        "text": item.text,
        "seconds": round(seconds, 1),
    })

    rec = _Recorder()
    try:
        _start_capture(rec)
        _collect_for(rec, seconds)
    except Exception as exc:  # noqa: BLE001 - missing device etc. → clean error
        rec.recording = False
        try:
            rec._stop_capture_streams()
        except Exception:  # noqa: BLE001 - teardown must not mask the error
            pass
        _print_event({"event": "corpus_record_error", "error": str(exc)})
        return 0

    rec.recording = False
    rec._stop_capture_streams()

    pcm = _frames_to_int16_mono(rec)
    if len(pcm) == 0:
        _print_event({
            "event": "corpus_record_error",
            "error": "no audio was captured (check the microphone)",
        })
        return 0

    out_path = appdata_audio_dir(appdata) / f"{item.id}.wav"
    try:
        _write_wav(out_path, pcm)
    except Exception as exc:  # noqa: BLE001 - disk/permission error → clean error
        _print_event({"event": "corpus_record_error", "error": str(exc)})
        return 0

    peak_dbfs, rms_dbfs = _peak_rms_dbfs(pcm)
    seconds_recorded = round(len(pcm) / float(vp_capture.SR), 2)
    _print_event({
        "event": "corpus_record_done",
        "id": item.id,
        "path": str(out_path),
        "seconds_recorded": seconds_recorded,
        "peak_dbfs": peak_dbfs,
        "rms_dbfs": rms_dbfs,
    })
    return 0
