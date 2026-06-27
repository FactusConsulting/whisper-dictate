"""Whisper transcription core — pure function plus hallucination filter.

Imports faster_whisper lazily inside _transcribe so the module is cheap to
import while the runtime module keeps the heavy DLL/CUDA bootstrap centralized.
"""
from __future__ import annotations

import os
import json
import re
import subprocess
import tempfile
import threading
import time
import wave
from dataclasses import dataclass, field
from importlib import resources
from typing import Any

import numpy as np

from whisper_dictate.vp_audio import (
    _boost_quiet,
    _boost_quiet_detail,
    _looks_like_speech,
    _trim_trailing_silence,
)
from whisper_dictate.vp_config import apply_config_to_environ, get_value
from whisper_dictate.vp_dictionary_store import default_dictionary_path

apply_config_to_environ()

SR = 16000

# Module-level lock that SERIALIZES every faster-whisper model.transcribe()
# call — both the final transcription at key release (_transcribe_detail) and
# the in-flight live previews (vp_preview). faster-whisper / CTranslate2 model
# objects are not safe to drive from two threads at once, so the preview thread
# and the final pass must never call transcribe concurrently. The final pass
# acquires it blocking (it MAY wait for at most one in-flight preview chunk —
# bounded, since a preview only transcribes the buffer captured so far); the
# preview path acquires it non-blocking so previews never queue up behind each
# other or behind the final pass.
TRANSCRIBE_LOCK = threading.Lock()

# beam_size=1 is fastest on CPU; raise to 5 for better accuracy at the
# cost of 3-4x slower transcription. VOICEPI_BEAM_SIZE=5 is useful on
# machines without GPU where accuracy matters more than latency.
BEAM_SIZE = int(get_value("VOICEPI_BEAM_SIZE", "1") or "1")

# Optional context hint fed to Whisper before each utterance. Improves
# recognition of domain-specific terms (product names, jargon, names).
INITIAL_PROMPT = get_value("VOICEPI_INITIAL_PROMPT") or None
# Set True when --prompt forces INITIAL_PROMPT for this run: the live config
# reload then leaves it alone, so the CLI override stays authoritative for the
# whole session instead of being overwritten by the saved config value (#154).
INITIAL_PROMPT_FORCED = False


def _parse_temperatures(spec: str | None) -> list[float]:
    # Comma-separated floats; "0.0,0.2" by default. Set "0.0" (or "0")
    # to lock Whisper to greedy decode — eliminates the fallback to
    # higher-temperature decodes that can produce more "creative"
    # (= less faithful) text when the greedy pass hits no_speech /
    # log_prob thresholds.
    raw = (spec or "0.0,0.2").strip()
    try:
        out = [float(p.strip()) for p in raw.split(",") if p.strip()]
    except ValueError:
        out = []
    return out or [0.0, 0.2]


# Whisper decode-temperature ladder. faster-whisper retries at the next
# temperature when the previous decode trips an internal no_speech /
# log_prob threshold. Lock to "0.0" via env for predictable output.
TEMPERATURES = _parse_temperatures(get_value("VOICEPI_TEMPERATURE"))

# Pass `condition_on_previous_text=True` only on utterances longer
# than CONTEXT_MIN_SECONDS. Defaults to 0 = always False (avoids
# Whisper hallucinating continuations on short/quiet input — what
# the HALLUCINATIONS set was added to filter). Set to e.g. 5 to opt
# long utterances into context-conditioned decode, which helps
# Whisper keep word boundaries coherent across segments.
CONTEXT_MIN_SECONDS = float(get_value("VOICEPI_CONTEXT_MIN_SECONDS", "5") or "5")
VAD_THRESHOLD = float(get_value("VOICEPI_VAD_THRESHOLD", "0.3") or "0.3")
VAD_MIN_SILENCE_MS = int(get_value("VOICEPI_VAD_MIN_SILENCE_MS", "600") or "600")
VAD_SPEECH_PAD_MS = int(get_value("VOICEPI_VAD_SPEECH_PAD_MS", "200") or "200")

# --- Trailing-silence hallucination guards (local Whisper only) ---
# When on (default), ask faster-whisper to skip long silent gaps where it tends
# to hallucinate "like and subscribe"-style text. This needs word timestamps
# (small extra compute). Toggled from the UI; a no-op for cloud STT, which
# never reaches this code path.
HALLUCINATION_GUARD = (get_value("VOICEPI_HALLUCINATION_GUARD", "1") or "1").strip().lower() not in (
    "", "0", "false", "no", "off")
HALLUCINATION_SILENCE_S = float(get_value("VOICEPI_HALLUCINATION_SILENCE_S", "2.0") or "2.0")
# Always-on segment scrub (cheap, no setting): drop a transcribed segment the
# model itself flags as very likely non-speech (high no_speech_prob AND low
# confidence), or whose end timestamp runs past the captured audio — the classic
# "like and subscribe" / repetition artifacts Whisper emits on trailing silence.
NO_SPEECH_DROP = float(get_value("VOICEPI_NO_SPEECH_DROP", "0.6") or "0.6")
NO_SPEECH_DROP_LOGPROB = float(get_value("VOICEPI_NO_SPEECH_DROP_LOGPROB", "-0.5") or "-0.5")
SEGMENT_END_SLACK_S = 1.0
# Speech-rate plausibility gate: a transcript whose chars-per-second exceeds
# this is humanly impossible (real speech is 15-25 chars/s) and is almost
# certainly a hallucinated credit/caption on quiet input. "0" disables the gate.
# Live-reloadable (mirrored from config in Dictate._apply_runtime_module_config).
MAX_CHARS_PER_SECOND = float(get_value("VOICEPI_MAX_CHARS_PER_SECOND", "30") or "30")
STT_DEBUG = (get_value("VOICEPI_STT_DEBUG") or "").strip().lower() not in (
    "", "0", "false", "no", "off")
# Wave 8 of #348 dropped the `"parakeet"` entry from this set together with
# the NeMo/Parakeet backend; a saved `stt_backend = "parakeet"` is migrated
# to whisper at config-load time (see Rust `config::load::migrate_parakeet_backend`).
VALID_STT_BACKENDS = ("whisper", "openai")
STT_BACKEND = (get_value("VOICEPI_STT_BACKEND", "whisper") or "whisper").strip().lower()
if STT_BACKEND == "faster-whisper":
    STT_BACKEND = "whisper"
# Belt-and-braces: if a stale environment still carries
# VOICEPI_STT_BACKEND=parakeet, fall back to the schema default rather than
# crashing on the validation below. The Rust UI dropped the option and the
# config loader migrates persistently, so this only matters when a Python
# worker is launched from a shell that exported the legacy env var by hand.
if STT_BACKEND == "parakeet":
    STT_BACKEND = "whisper"

# Phase 1.2 of the Python-removal roadmap (#348): when this env var is set to
# "rust" AND the Rust binary was compiled with `--features whisper-rs-local`,
# the local Whisper backend dispatches through the Rust helper subprocess
# instead of the in-process faster-whisper bindings. Any other value (or
# unset) keeps the existing behaviour byte-identically — gate is checked
# inside load_stt_model so a live config change picks up next utterance.
TRANSCRIBE_BACKEND_ENV = "VOICEPI_TRANSCRIBE_BACKEND"
TRANSCRIBE_BACKEND_RUST = "rust"

_DictionaryCacheKey = tuple[str | None, str | None, str | None, str | None, str | None, str]
_DICTIONARY_PROMPT_CACHE: dict[_DictionaryCacheKey, DictionaryRuntimeResult] = {}


def _local_only_enabled() -> bool:
    return (get_value("VOICEPI_LOCAL_ONLY") or "").strip().lower() not in (
        "", "0", "false", "no", "off")


def _is_loopback_url(url: str | None) -> bool:
    """True when an HTTP(S) URL targets the local machine (loopback).

    A self-hosted endpoint on loopback never leaves the box, so it stays
    compatible with VOICEPI_LOCAL_ONLY. Mirrors the Rust privacy helper.
    """
    if not url:
        return False
    authority = url.split("://", 1)[-1].split("/", 1)[0]
    host_port = authority.rsplit("@", 1)[-1]  # strip any user:pass@
    if host_port.startswith("["):  # [::1]:port
        host = host_port[1:].split("]", 1)[0]
    else:
        host = host_port.split(":", 1)[0]
    host = host.strip().lower()
    return host in ("localhost", "::1") or host == "127.0.0.1" or host.startswith("127.")


def _rust_privacy_ok(helper: str, backend: str, feature: str,
                     base_url: str | None = None) -> bool:
    """Ask the Rust privacy helper whether ``backend`` is allowed.

    Returns True when the helper explicitly approves; raises RuntimeError when
    it explicitly rejects; returns False when the helper is unavailable or its
    answer is unusable (the caller then applies the Python fallback check).
    """
    try:
        r = subprocess.run(
            [helper, "privacy"],
            input=json.dumps({
                "action": "assert_backend",
                "local_only": _local_only_enabled(),
                "backend": backend,
                "feature": feature,
                "base_url": base_url or "",
            }),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=5,
            shell=False,
        )
    except Exception:  # noqa: BLE001 - fall back to the Python check
        return False
    if r.returncode != 0:
        return False
    try:
        payload = json.loads(r.stdout or "{}")
    except json.JSONDecodeError:
        return False
    if not isinstance(payload, dict):
        return False
    if not payload.get("ok", False):
        raise RuntimeError(str(payload.get("error") or "local-only check failed"))
    return True


def _assert_local_backend(backend: str, *, feature: str = "STT") -> None:
    base_url = get_value("VOICEPI_STT_BASE_URL")
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if helper and _rust_privacy_ok(helper, backend, feature, base_url):
        return
    if (_local_only_enabled()
            and (backend or "").strip().lower() not in ("whisper", "faster-whisper")
            and not _is_loopback_url(base_url)):
        raise RuntimeError(
            f"VOICEPI_LOCAL_ONLY=1 blocks {feature} backend {backend!r}; "
            "choose a local backend, a loopback endpoint, or disable local-only mode.")


@dataclass
class _RustSegment:
    """Minimal segment shim mirroring the duck-typed surface that
    ``_transcribe_detail`` / ``_drop_hallucinated_segments`` read off
    faster-whisper segments.

    The Rust backend returns only the concatenated transcript text (no
    per-segment timings or confidences yet — those are out of scope for the
    Phase 1.2 wiring; richer metadata lands once we move the post-flow into
    Rust too). The downstream Python code tolerates missing attributes via
    ``getattr(..., default)``, so leaving optional fields as ``None`` is
    enough to keep the existing pipeline working.
    """
    text: str
    start: float | None = None
    end: float | None = None
    avg_logprob: float = 0.0
    no_speech_prob: float = 0.0


@dataclass
class _RustInfo:
    language: str | None = None
    language_probability: float | None = None


def _rust_helper_binary() -> str | None:
    """Resolve the whisper-dictate binary used as the Rust transcription helper.

    Reuses ``VOICEPI_RUST_INJECTOR`` (set by the supervisor for every Python
    worker — see runtime.py) so we don't introduce a second discovery path.
    Returns ``None`` when the helper isn't available; the caller falls back
    to in-process faster-whisper.
    """
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    return helper or None


def _rust_transcribe_enabled() -> bool:
    """True when the runtime opted into the Rust transcription backend.

    Checked fresh on every model load so a config edit + reload picks up the
    flip without restarting the supervisor.
    """
    value = (os.environ.get(TRANSCRIBE_BACKEND_ENV) or "").strip().lower()
    return value == TRANSCRIBE_BACKEND_RUST


def _rust_helper_supports_transcribe(helper: str, *, timeout_s: float = 10.0) -> bool:
    """Probe ``whisper-dictate transcribe-wav --probe`` and return True iff the
    helper exits zero.

    The supervisor sets ``VOICEPI_RUST_INJECTOR`` for every Python worker even
    on a stock build (it's the same binary used for redact/profile/etc.), so
    presence of the env var does NOT imply this binary was compiled with the
    ``whisper-rs-local`` feature. Without this probe a stock build paired with
    ``VOICEPI_TRANSCRIBE_BACKEND=rust`` would happily select
    :class:`RustWhisperShellModel` and then fail the FIRST dictation when the
    real shell-out exits non-zero with "feature not compiled in" — far worse
    than detecting the mismatch up-front and quietly using faster-whisper.

    Any error (binary missing, timeout, non-zero exit) is treated as "not
    supported" — we'd rather fall back silently than refuse to dictate.
    """
    try:
        r = subprocess.run(
            [helper, "transcribe-wav", "--probe"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout_s,
            shell=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return r.returncode == 0


def _write_temp_wav_16khz_mono(audio: np.ndarray) -> str:
    """Materialise ``audio`` (float32 in [-1, 1]) to a 16 kHz mono int16 WAV
    on disk and return the path.

    Materialises a 16 kHz mono int16 WAV so the Rust helper's
    `decode_wav_16k_mono` accepts it. Caller is responsible for deleting the
    file.
    """
    fd, path = tempfile.mkstemp(prefix="voicepi-rust-stt-", suffix=".wav")
    os.close(fd)
    clipped = np.clip(audio, -1.0, 1.0)
    pcm = (clipped * 32767.0).astype(np.int16)
    with wave.open(path, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SR)
        wav.writeframes(pcm.tobytes())
    return path


class RustWhisperShellModel:
    """faster-whisper-shaped wrapper around the Rust ``transcribe-wav`` helper.

    Implements the subset of the faster-whisper ``WhisperModel.transcribe``
    surface that ``_transcribe_detail`` and ``transcribe_preview`` actually
    consume: a single ``transcribe(audio, language=..., initial_prompt=..., ...)``
    call returning ``(segments_iter, info)``. All other faster-whisper kwargs
    (beam size, temperatures, VAD parameters, the hallucination guards) are
    silently accepted and ignored — the Rust helper currently exposes only
    language + initial prompt, and the Python-side post-filters
    (``_drop_hallucinated_segments``, dictionary replacements, speech-rate
    guard) still run unchanged on the returned text.

    Keeping the wrapper here rather than building a separate runtime branch
    means the Phase 1.2 toggle is a single env-var check inside
    ``load_stt_model`` — every other piece of the transcription pipeline
    (preview thread, locking, telemetry) keeps working without changes.
    """

    def __init__(self, helper: str):
        self._helper = helper

    def transcribe(
        self,
        audio: np.ndarray,
        *,
        language: str | None = None,
        initial_prompt: str | None = None,
        **_ignored: Any,
    ):
        # Materialise the in-memory audio to a temp WAV — the helper's
        # decoder is the only on-disk loader we have right now (Phase 1.2
        # explicitly reuses the WAV-on-disk IPC the audio capture already
        # uses; raw-frame IPC is a later optimisation in Wave 7).
        wav_path = _write_temp_wav_16khz_mono(
            audio.reshape(-1).astype(np.float32))
        try:
            payload = _run_rust_transcribe(
                self._helper, wav_path,
                language=language,
                initial_prompt=initial_prompt,
            )
        finally:
            try:
                os.remove(wav_path)
            except OSError:
                pass
        text = str(payload.get("text", ""))
        # Trim surrounding whitespace so the Python concatenation logic in
        # ``_transcribe_detail`` (``re.sub(r"\s+", " ", ...)``) still produces
        # the same output shape as a faster-whisper single-segment return.
        return [_RustSegment(text=text)], _RustInfo()


class RustWhisperServerModel:
    """Long-running in-process wrapper around ``whisper-dictate transcribe-server``.

    Wave 8-A of #348: spawn the Rust helper ONCE per supervisor lifetime and
    reuse the live subprocess for every transcribe call, so the GGML model is
    loaded once and stays resident between utterances (subject to
    ``VOICEPI_WHISPER_IDLE_UNLOAD_S``). Previously
    :class:`RustWhisperShellModel` paid the cold-start cost (75 MB to 1.5 GB
    model load) on EVERY utterance because each call spawned a fresh
    ``transcribe-wav`` subprocess.

    Same faster-whisper-shaped ``transcribe(audio, ...)`` surface as
    :class:`RustWhisperShellModel`, so ``load_stt_model`` can swap in this
    wrapper without touching the rest of the pipeline.

    Thread-safe: a single ``threading.Lock`` around the stdin write + stdout
    readline pair so concurrent preview + final transcribe calls (the only
    realistic source of contention) cannot interleave their JSON envelopes
    on the wire and tear up the response stream.
    """

    # Per-request response deadline. whisper.cpp inference scales with audio
    # length and model size (~1x realtime on CPU for tiny, slower for large);
    # 300 s leaves headroom for the longest reasonable dictation and matches
    # the historical RustWhisperShellModel `subprocess.run(..., timeout=300)`
    # ceiling. Configurable via the env var so a slow-GPU user can extend it
    # without a code change.
    _DEFAULT_RESPONSE_TIMEOUT_S = float(
        get_value("VOICEPI_TRANSCRIBE_SERVER_TIMEOUT_S", "300") or "300"
    )

    def __init__(self, helper: str):
        self._helper = helper
        self._lock = threading.Lock()
        self._response_timeout_s = self._DEFAULT_RESPONSE_TIMEOUT_S
        # `_dead` is sticky once set so a single death triggers exactly ONE
        # respawn attempt on the next transcribe call (via _ensure_alive).
        # We do NOT auto-respawn inside the failing call itself — surfacing
        # the failure once lets the dictation loop log and retry cleanly.
        self._dead = False
        self._proc = self._spawn()
        try:
            self._ready = self._read_ready_line()
        except Exception:
            # If the ready handshake fails, tear the child down so we don't
            # leak a half-started server when the caller catches and falls
            # back to RustWhisperShellModel.
            self.close()
            raise

    def _spawn(self) -> subprocess.Popen:
        """Spawn a fresh helper subprocess with the documented IPC shape.

        Pulled out so ``_ensure_alive`` can re-spawn after a death without
        duplicating the Popen kwargs. Per Codex P2 on #395: ``stderr`` is
        redirected to DEVNULL because whisper.cpp writes verbose model-load
        + per-segment debug there, and a PIPE'd stderr that we never drain
        eventually fills the OS pipe buffer (~64 KB) — at which point the
        helper blocks on its own stderr write and the JSON response we're
        waiting on deadlocks. Discarding stderr keeps the response path
        unblockable. Errors still reach us via the per-request
        ``{"error":"..."}`` envelope on stdout, so we only lose the
        non-actionable debug banner.
        """
        return subprocess.Popen(  # noqa: S603 - trusted helper path
            [self._helper, "transcribe-server"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,  # line-buffered stdout so readline() doesn't block forever
            shell=False,
        )

    # Bounded handshake deadline so a wedged helper at startup (model load
    # stuck, GPU init hang, ...) doesn't block `load_stt_model` forever.
    # 60 s comfortably covers a cold large-v3 load on commodity CPU; a
    # slower setup can extend it via the env var.
    _DEFAULT_READY_TIMEOUT_S = float(
        get_value("VOICEPI_TRANSCRIBE_SERVER_READY_TIMEOUT_S", "60") or "60"
    )

    def _read_ready_line(self) -> dict:
        """Read and parse the first stdout line — a ``ServerReady`` envelope.

        Raises ``RuntimeError`` on:
        - EOF before any output (the helper crashed before getting to the
          ready emit),
        - timeout (the helper is alive but never wrote the line — model
          load wedge or pre-flush hang),
        - invalid JSON,
        - missing ``ready: true`` field.

        Each of these maps to a clear log line on the caller side so the
        fallback to per-utterance shell-out is auditable.

        Per Codex P2 on #395: previously this read had no deadline and a
        feature-enabled helper that never wrote the ready line blocked
        `load_stt_model` forever — the per-request timeout only kicked in
        for response reads. Now both the handshake AND the per-request
        reads share `_read_response_line_with_timeout` (separate
        defaults).
        """
        assert self._proc.stdout is not None  # mypy/pylance — PIPE = guaranteed
        line = self._read_response_line_with_timeout(self._DEFAULT_READY_TIMEOUT_S)
        if not line:
            raise RuntimeError(
                "transcribe-server exited or timed out before emitting "
                "ready line (within {:.0f}s)".format(self._DEFAULT_READY_TIMEOUT_S)
            )
        try:
            payload = json.loads(line)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"transcribe-server ready line is not valid JSON: {line!r}: {e}"
            ) from e
        if not isinstance(payload, dict) or not payload.get("ready"):
            raise RuntimeError(
                f"transcribe-server first line missing ready=true: {payload!r}"
            )
        return payload

    def _ensure_alive(self) -> None:
        """Respawn the helper if the previous call marked it dead OR the
        OS reports it has already exited.

        Per Codex P2 on #395: previously a single EOF / BrokenPipe left
        ``self._proc`` pointing at a dead process; every subsequent
        transcribe call wrote to a closed stdin and the user lost
        dictation until the supervisor restarted. The fix respawns the
        helper transparently — the user pays ONE failed utterance (the
        one that caused the death), then dictation continues.
        """
        if not self._dead and self._proc.poll() is None:
            return
        # Clean up the corpse before spawning a replacement.
        try:
            self.close()
        except Exception:  # noqa: BLE001 - best-effort cleanup, keep going
            pass
        self._proc = self._spawn()
        self._ready = self._read_ready_line()
        self._dead = False

    def transcribe(
        self,
        audio: np.ndarray,
        *,
        language: str | None = None,
        initial_prompt: str | None = None,
        **_ignored: Any,
    ):
        # Respawn first if the helper died on a previous call — this
        # is the only place that can recover from a transient crash
        # without losing the model's residency for new calls.
        self._ensure_alive()
        wav_path = _write_temp_wav_16khz_mono(audio.reshape(-1).astype(np.float32))
        try:
            payload = self._exchange(wav_path, language, initial_prompt)
        finally:
            try:
                os.remove(wav_path)
            except OSError:
                pass
        if "error" in payload:
            raise RuntimeError(
                f"Rust transcribe-server error: {payload['error']}"
            )
        text = str(payload.get("text", ""))
        return [_RustSegment(text=text)], _RustInfo()

    def _exchange(
        self,
        wav_path: str,
        language: str | None,
        initial_prompt: str | None,
    ) -> dict:
        """Send one JSON request to the server and read one JSON response.

        The lock guarantees that a concurrent preview thread cannot
        interleave its request mid-write or steal the response line
        belonging to another caller. Pipe errors (the server crashed) and
        unexpected EOFs / timeouts surface as ``RuntimeError`` and mark
        the worker dead so the next call respawns via ``_ensure_alive``.

        Per Codex P2 on #395: the response read is bounded by
        ``_read_response_line_with_timeout`` (default 300 s, configurable
        via ``VOICEPI_TRANSCRIBE_SERVER_TIMEOUT_S``) so a wedged
        whisper.cpp call cannot hold ``TRANSCRIBE_LOCK`` indefinitely
        — the worst case is one stuck utterance and a fresh subprocess.
        """
        request = {
            "action": "transcribe_wav",
            "wav_path": wav_path,
            "language": language or "",
            "initial_prompt": initial_prompt or "",
        }
        encoded = json.dumps(request, ensure_ascii=False) + "\n"
        with self._lock:
            assert self._proc.stdin is not None
            assert self._proc.stdout is not None
            try:
                self._proc.stdin.write(encoded)
                self._proc.stdin.flush()
            except (BrokenPipeError, OSError) as e:
                self._dead = True
                raise RuntimeError(
                    f"failed to write transcribe-server request: {e}"
                ) from e
            line = self._read_response_line_with_timeout(self._response_timeout_s)
        if not line:
            self._dead = True
            raise RuntimeError(
                "transcribe-server exited or timed out mid-call (no response "
                "line within {:.0f}s); the worker has been marked dead and "
                "the next call will respawn it".format(self._response_timeout_s)
            )
        try:
            return json.loads(line)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"transcribe-server response is not valid JSON: {line!r}: {e}"
            ) from e

    def _read_response_line_with_timeout(self, timeout_s: float) -> str:
        """Run the subprocess's stdout.readline() with a bounded deadline.

        Cross-platform implementation: ``select`` doesn't work on Windows
        anonymous pipes, so we run the blocking ``readline`` in a daemon
        thread and join with the timeout. On timeout we ``kill()`` the
        subprocess — the killed process closes stdout, the reader
        unblocks, and the caller sees the empty line as a
        timeout-equivalent EOF (handled by the empty-line check in
        ``_exchange``). Returns the line on success, "" on timeout/EOF.

        Per Codex P2 on #395: the proc + stdout handles are captured into
        LOCALS before the reader thread starts, then both kill() and the
        reader use those locals. Otherwise a slow-to-start reader could
        outlive a timeout + ``_ensure_alive`` respawn cycle and steal the
        ready/response line of the REPLACEMENT subprocess (the closure
        would dereference ``self._proc`` at execution time, which by then
        points at the new helper).
        """
        proc = self._proc
        stdout = proc.stdout
        result: list[str] = [""]

        def _reader() -> None:
            try:
                result[0] = stdout.readline() or ""
            except Exception:  # noqa: BLE001 - any reader failure -> empty
                result[0] = ""

        t = threading.Thread(target=_reader, daemon=True)
        t.start()
        t.join(timeout=timeout_s)
        if t.is_alive():
            # Reader still blocked -> kill THIS subprocess (not whatever
            # self._proc happens to point at now — see the comment above).
            # The killed proc's stdout closes, the reader unblocks, and
            # this method returns "" so the caller marks the worker dead.
            try:
                proc.kill()
            except OSError:
                pass
            return ""
        return result[0]

    @property
    def ready(self) -> dict:
        """The parsed ServerReady envelope. Exposed for diagnostics
        (UI/log) and tests that want to assert the resolved model path or
        idle timeout the server actually adopted."""
        return dict(self._ready)

    def close(self) -> None:
        """Close stdin (EOF triggers a clean exit on the Rust side) and
        wait for the child. Force-kill after a short grace period so a
        wedged worker can't block supervisor shutdown.
        """
        if self._proc.poll() is not None:
            return
        try:
            if self._proc.stdin is not None:
                self._proc.stdin.close()
        except OSError:
            pass
        try:
            self._proc.wait(timeout=5.0)
        except subprocess.TimeoutExpired:
            self._proc.kill()
            try:
                self._proc.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                pass

    def __del__(self):  # noqa: D401 - best-effort cleanup, no docstring needed
        try:
            self.close()
        except Exception:  # noqa: BLE001 - destructor must not raise
            pass


def _run_rust_transcribe(
    helper: str,
    wav_path: str,
    *,
    language: str | None,
    initial_prompt: str | None,
    timeout_s: float = 300.0,
) -> dict:
    """Invoke ``whisper-dictate transcribe-wav`` and return the parsed JSON.

    Raises ``RuntimeError`` on any failure (process error, non-JSON output,
    or non-dict payload). Bubbling these up rather than silently falling back
    matches the explicit-opt-in design of ``VOICEPI_TRANSCRIBE_BACKEND=rust``:
    the user asked for the Rust backend, so if it can't deliver, fail loudly
    instead of silently re-using the Python path and producing different
    results.
    """
    request = {
        "action": "transcribe_wav",
        "wav_path": wav_path,
        "language": language or "",
        "initial_prompt": initial_prompt or "",
    }
    try:
        r = subprocess.run(
            [helper, "transcribe-wav"],
            input=json.dumps(request, ensure_ascii=False),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout_s,
            shell=False,
        )
    except subprocess.TimeoutExpired as e:
        raise RuntimeError(
            f"Rust transcribe-wav timed out after {timeout_s:.0f}s") from e
    except OSError as e:
        raise RuntimeError(f"failed to launch Rust transcribe-wav: {e}") from e
    if r.returncode != 0:
        err = (r.stderr or "").strip() or f"exit {r.returncode}"
        raise RuntimeError(f"Rust transcribe-wav failed: {err}")
    raw = (r.stdout or "").strip()
    if not raw:
        raise RuntimeError("Rust transcribe-wav returned empty stdout")
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"Rust transcribe-wav returned invalid JSON: {e}") from e
    if not isinstance(payload, dict):
        raise RuntimeError(
            f"Rust transcribe-wav returned non-object payload: {type(payload).__name__}")
    return payload


def load_stt_model(model_name: str, device: str, compute_type: str):
    """Load the selected STT backend lazily.

    The default path preserves the existing faster-whisper behaviour. The
    NVIDIA Parakeet backend was removed in Wave 8 of #348 — use the local
    Whisper backend or the OpenAI-compatible cloud backend.

    When ``VOICEPI_TRANSCRIBE_BACKEND=rust`` is set AND the local Whisper
    backend is selected (``VOICEPI_STT_BACKEND=whisper``, the default), the
    in-process faster-whisper bindings are replaced by a wrapper that shells
    out to ``whisper-dictate transcribe-wav`` — Phase 1.2 of the
    Python-removal roadmap (#348). The Rust path needs ``VOICEPI_RUST_INJECTOR``
    to point at a whisper-dictate binary built with
    ``--features whisper-rs-local``; if either is missing we fall through to
    the in-process path so a misconfigured override doesn't break dictation.
    """
    backend = STT_BACKEND
    _assert_local_backend(backend)
    if backend not in VALID_STT_BACKENDS:
        raise ValueError(
            "invalid VOICEPI_STT_BACKEND="
            f"{backend!r}; expected one of {', '.join(VALID_STT_BACKENDS)}")
    if backend == "openai":
        from whisper_dictate.vp_external_api import ExternalTranscriptionModel
        return ExternalTranscriptionModel(model_name)
    if backend == "whisper" and _rust_transcribe_enabled():
        helper = _rust_helper_binary()
        if helper and _rust_helper_supports_transcribe(helper):
            # Wave 8-A of #348: prefer the long-running in-process
            # ``transcribe-server`` (the model loads ONCE and stays resident
            # between utterances) over per-utterance ``transcribe-wav``.
            # On a binary that doesn't ship the server subcommand the spawn
            # fails fast at the ready-line read and we fall through to the
            # legacy ShellModel — same robustness behaviour as the
            # `--probe` fallback to faster-whisper below.
            try:
                model = RustWhisperServerModel(helper)
                print(
                    f"[stt] {TRANSCRIBE_BACKEND_ENV}={TRANSCRIBE_BACKEND_RUST}: "
                    f"dispatching local Whisper through long-running Rust "
                    f"server (model={model.ready.get('model_path', '?')!r}, "
                    f"idle_unload_s={model.ready.get('idle_unload_s', 0)})",
                    flush=True,
                )
                return model
            except Exception as e:  # noqa: BLE001 - any spawn/probe failure falls back
                print(
                    f"[stt] transcribe-server unavailable ({e}); "
                    f"falling back to per-utterance transcribe-wav",
                    flush=True,
                )
            print(
                f"[stt] {TRANSCRIBE_BACKEND_ENV}={TRANSCRIBE_BACKEND_RUST}: "
                f"dispatching local Whisper through Rust helper",
                flush=True,
            )
            return RustWhisperShellModel(helper)
        # Fall through to faster-whisper with a diagnostic — the explicit
        # opt-in was honoured at the env-var level but the helper is either
        # missing or doesn't support `transcribe-wav` (stock build without the
        # whisper-rs-local feature). Log clearly so the user knows the env
        # var was effectively ignored.
        if not helper:
            reason = "VOICEPI_RUST_INJECTOR not set"
        else:
            reason = (
                f"helper at {helper!r} does not support transcribe-wav "
                "(stock build without whisper-rs-local)"
            )
        print(
            f"[stt] {TRANSCRIBE_BACKEND_ENV}={TRANSCRIBE_BACKEND_RUST} ignored: "
            f"{reason}; using in-process faster-whisper",
            flush=True,
        )
    from faster_whisper import WhisperModel
    return WhisperModel(model_name, device=device, compute_type=compute_type)


@dataclass
class TranscribeResult:
    text: str
    raw_text: str = ""
    duration_s: float = 0.0
    post_boost_dbfs: float | None = None
    raw_dbfs: float | None = None
    peak: float | None = None
    gain: float | None = None
    noise_dbfs: float | None = None
    snr_db: float | None = None
    input_status: str = ""
    compute_s: float = 0.0
    real_time_factor: float | None = None
    language: str | None = None
    language_probability: float | None = None
    segments: list[dict[str, Any]] = field(default_factory=list)
    gate: str = ""
    dictionary_terms: list[str] = field(default_factory=list)
    dictionary_replacements: list[dict[str, object]] = field(default_factory=list)


@dataclass
class DictionaryRuntimeResult:
    text: str = ""
    prompt: str | None = None
    terms: list[str] = field(default_factory=list)
    replacements: list[dict[str, str]] = field(default_factory=list)
    changes: list[dict[str, object]] = field(default_factory=list)
    term_count: int = 0
    replacement_count: int = 0
    path: str | None = None
    error: str | None = None
    enabled: bool = False


def _base_prompt_only(base_prompt: str | None) -> str | None:
    prompt = (base_prompt or "").strip()
    return prompt or None


def _run_dictionary_helper_payload(text: str, base_prompt: str | None) -> dict | None:
    """Run the Rust dictionary-runtime helper and return its parsed dict.

    Returns None when the helper is unavailable, exits non-zero, or returns
    unparseable / non-dict output — the caller falls back in every such case.
    """
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if not helper:
        return None
    try:
        r = subprocess.run(
            [helper, "dictionary-runtime"],
            input=json.dumps({
                "base_prompt": base_prompt,
                "text": text,
            }, ensure_ascii=False),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=5,
            shell=False,
        )
    except Exception as e:  # noqa: BLE001 - dictation must survive helper trouble
        if STT_DEBUG:
            print(f"[dictionary] Rust helper error: {e}", flush=True)
        return None
    if r.returncode != 0:
        if STT_DEBUG:
            err = (r.stderr or "").strip()
            print(f"[dictionary] Rust helper failed: {err}", flush=True)
        return None
    try:
        payload = json.loads(r.stdout or "{}")
    except json.JSONDecodeError as e:
        if STT_DEBUG:
            print(f"[dictionary] Rust helper returned invalid JSON: {e}", flush=True)
        return None
    return payload if isinstance(payload, dict) else None


def _parse_dictionary_changes(payload: dict) -> list[dict[str, object]]:
    changes = []
    for item in payload.get("changes") or []:
        if not isinstance(item, dict):
            continue
        changes.append({
            "from": str(item.get("from") or ""),
            "to": str(item.get("to") or ""),
            "count": int(item.get("count") or 0),
        })
    return changes


def _parse_dictionary_replacements(payload: dict) -> list[dict[str, str]]:
    replacements = []
    for item in payload.get("replacements") or []:
        if not isinstance(item, dict):
            continue
        replacements.append({
            "from": str(item.get("from") or ""),
            "to": str(item.get("to") or ""),
        })
    return replacements


def _dictionary_runtime(text: str = "", base_prompt: str | None = None) -> DictionaryRuntimeResult:
    fallback = DictionaryRuntimeResult(text=text, prompt=_base_prompt_only(base_prompt))
    payload = _run_dictionary_helper_payload(text, base_prompt)
    if payload is None:
        return fallback

    error = payload.get("error")
    if error and STT_DEBUG:
        print(f"[dictionary] {error}", flush=True)

    return DictionaryRuntimeResult(
        text=str(payload.get("text", text)),
        prompt=payload.get("prompt") if isinstance(payload.get("prompt"), str) else None,
        terms=[str(term) for term in (payload.get("terms") or [])],
        replacements=_parse_dictionary_replacements(payload),
        changes=_parse_dictionary_changes(payload),
        term_count=int(payload.get("term_count") or 0),
        replacement_count=int(payload.get("replacement_count") or 0),
        path=str(payload["path"]) if payload.get("path") else None,
        error=str(error) if error else None,
        enabled=bool(payload.get("enabled", False)),
    )


def _dictionary_cache_key(base_prompt: str | None) -> _DictionaryCacheKey:
    def env_value(env: str, default: str | None = None) -> str | None:
        value = os.environ.get(env)
        return value if value not in (None, "") else default

    def dictionary_freshness(raw_paths: str | None) -> str:
        if raw_paths:
            paths = [part for part in raw_paths.split(os.pathsep) if part]
        else:
            paths = [str(default_dictionary_path())]
        parts = []
        for raw_path in paths:
            path = os.path.expanduser(raw_path)
            try:
                stat = os.stat(path)
                parts.append(f"{path}\0{stat.st_mtime_ns}\0{stat.st_size}")
            except OSError:
                parts.append(f"{path}\0missing")
        return "\0\0".join(parts)

    dictionary_path = env_value("VOICEPI_DICTIONARY")
    return (
        base_prompt,
        env_value("VOICEPI_DICTIONARY_ENABLED", "1"),
        dictionary_path,
        env_value("VOICEPI_DICTIONARY_MAX_TERMS", "80"),
        env_value("VOICEPI_DICTIONARY_PROMPT_CHARS", "1200"),
        dictionary_freshness(dictionary_path),
    )


def _dictionary_prompt_runtime(base_prompt: str | None) -> DictionaryRuntimeResult:
    key = _dictionary_cache_key(base_prompt)
    cached = _DICTIONARY_PROMPT_CACHE.get(key)
    if cached is not None:
        return cached
    result = _dictionary_runtime("", base_prompt)
    _DICTIONARY_PROMPT_CACHE[key] = result
    return result


def _apply_cached_dictionary_runtime(
    text: str,
    base_prompt: str | None,
) -> DictionaryRuntimeResult | None:
    cached = _DICTIONARY_PROMPT_CACHE.get(_dictionary_cache_key(base_prompt))
    if cached is None or not cached.enabled:
        return None
    out = text
    changes: list[dict[str, object]] = []
    replacements = sorted(
        cached.replacements,
        key=lambda item: len(item.get("from", "")),
        reverse=True,
    )
    for replacement in replacements:
        source = replacement.get("from", "")
        target = replacement.get("to", "")
        if not source:
            continue
        pattern = (
            r"(^|[^\w])("
            + re.escape(source)
            + r")([^\w]|$)"
        )
        count = 0

        def _replace(match: re.Match[str]) -> str:
            nonlocal count
            count += 1
            return f"{match.group(1)}{target}{match.group(3)}"

        rewritten = re.sub(pattern, _replace, out, flags=re.IGNORECASE)
        if count:
            out = rewritten
            changes.append({
                "from": source,
                "to": target,
                "count": count,
            })
    return DictionaryRuntimeResult(
        text=out,
        prompt=cached.prompt,
        terms=cached.terms,
        replacements=cached.replacements,
        changes=changes,
        term_count=cached.term_count,
        replacement_count=cached.replacement_count,
        path=cached.path,
        error=cached.error,
        enabled=True,
    )

# Anti-hallucination PATTERN DATA lives in data/hallucination_patterns.json (see
# that file's _comment for the full design); this module owns only the matching
# LOGIC. The loader below rebuilds, byte-for-byte, the same HALLUCINATIONS
# frozenset and anchored _CREDIT_RE the inline literals used to produce: exact
# whole-text blacklist, plus a credit regex whose phrase prefixes require a
# trailing year (so real dictation starting with them survives) and whose bare
# company names match with an optional year.
_PATTERNS_RESOURCE = "hallucination_patterns.json"


def _load_hallucination_patterns() -> dict:
    """Read the pattern-data JSON once at import via importlib.resources.

    importlib.resources (not a filesystem path) stays robust across the
    zip/installed layouts the app ships in.
    """
    raw = (
        resources.files("whisper_dictate.data")
        .joinpath(_PATTERNS_RESOURCE)
        .read_text(encoding="utf-8")
    )
    return json.loads(raw)


def _build_credit_re(patterns: dict) -> re.Pattern[str]:
    """Compile the anchored subtitle-credit regex from the pattern data.

    The shared year tail is appended ONCE to the whole credit_phrase_prefixes
    alternation; each bare_company_names entry is an independent branch carrying
    its own optional year suffix. Anchored both ends, trailing punctuation/space
    tolerated.
    """
    phrase_group = "|".join(patterns["credit_phrase_prefixes"])
    body = "(?:" + "|".join([
        f"(?:{phrase_group}){patterns['credit_phrase_year_tail']}",
        *patterns["bare_company_names"],
    ]) + ")"
    return re.compile(rf"^{body}[\s.!?]*$", re.IGNORECASE)


def _build_credit_prefix_re(patterns: dict) -> re.Pattern[str]:
    """Compile a START-anchored regex of the credit-phrase prefixes WITHOUT the
    year requirement — a match means the text merely BEGINS like a subtitle
    credit ("Danske tekster af …"). This is intentionally loose: it also matches
    real dictation such as "danske tekster af høj kvalitet", so callers MUST pair
    it with a second, independent signal (an impossible speech rate) before
    dropping anything — see ``_drop_hallucinated_segments``. It exists to catch
    year-LESS credit hallucinations that ``_CREDIT_RE`` deliberately won't match
    on text alone (e.g. "Danske tekster af Nicolai Winther").
    """
    phrase_group = "|".join(patterns["credit_phrase_prefixes"])
    return re.compile(rf"^(?:{phrase_group})", re.IGNORECASE)


_HALLUCINATION_PATTERNS = _load_hallucination_patterns()
HALLUCINATIONS: frozenset[str] = frozenset(_HALLUCINATION_PATTERNS["exact_blacklist"])
_CREDIT_RE = _build_credit_re(_HALLUCINATION_PATTERNS)
_CREDIT_PREFIX_RE = _build_credit_prefix_re(_HALLUCINATION_PATTERNS)


def _looks_like_credit(text: str) -> bool:
    """True when the WHOLE text is a subtitle/caption credit (anchored match)."""
    return bool(_CREDIT_RE.match(text.strip().lower()))


def _looks_like_credit_prefix(text: str) -> bool:
    """True when the text BEGINS with a subtitle-credit phrase (year-less, loose).

    Only safe to act on together with an impossible-rate signal; on its own it
    matches real dictation that happens to start with these phrases.
    """
    return bool(_CREDIT_PREFIX_RE.match(text.strip().lower()))


def is_hallucination(text: str) -> bool:
    return text.lower().rstrip() in HALLUCINATIONS or _looks_like_credit(text)


def _drop_hallucinated_segments(segment_list, audio_duration_s):
    """Split segments into (kept, dropped), dropping trailing-silence
    hallucinations before the text is assembled.

    A segment is dropped when:

    * the model itself flags it as very likely non-speech (``no_speech_prob``
      high AND ``avg_logprob`` low together); or
    * its end timestamp runs past the captured audio (a hallucination beyond the
      recording, e.g. a 30 s "like and subscribe" tail on a 35 s clip); or
    * its OWN text is a known subtitle/caption credit (``is_hallucination`` —
      the exact-match blacklist plus the year-anchored credit regex) AND the
      model corroborates non-speech via a high ``no_speech_prob``
      (``>= NO_SPEECH_DROP``). This catches a credit emitted as a SEPARATE
      segment appended to a long real utterance — the whole assembled text is
      then not a credit, so the whole-text ``is_hallucination`` check downstream
      never sees it. The silence gate is required: the credit regex is loose
      enough (year-anchored, up to 60 leading chars) to match confident real
      dictation like "oversat af Google i 2023", so text shape alone must never
      drop a segment; or
    * its text is a known credit OR merely BEGINS with a credit prefix
      ("Danske tekster af …") AND its OWN char-rate is humanly impossible (e.g.
      the 60-char subtitle credit on a 0.30 s tail = ~200 chars/s). This needs NO
      ``no_speech_prob`` corroboration: the two independent signals (credit shape
      AND impossible speech rate) never co-occur on real speech, so it stays
      safe. It catches both the moderate-``no_speech`` repro the ``credit_pattern``
      gate misses (observed 0.43 < ``NO_SPEECH_DROP``) AND year-LESS credits the
      year-anchored ``is_hallucination`` regex deliberately won't match on text
      alone (e.g. "Danske tekster af Nicolai Winther" — the prefix matches real
      dictation too, hence the impossible-rate pairing).

    Returns (kept, dropped). ``dropped`` items are tagged with a transient
    ``_drop_reason`` attribute for diagnostics.
    """
    kept = []
    dropped = []
    for segment in segment_list:
        no_speech = float(getattr(segment, "no_speech_prob", 0.0) or 0.0)
        avg_logprob = float(getattr(segment, "avg_logprob", 0.0) or 0.0)
        start = getattr(segment, "start", None)
        end = getattr(segment, "end", None)
        seg_text = getattr(segment, "text", "") or ""
        likely_silence = no_speech >= NO_SPEECH_DROP and avg_logprob <= NO_SPEECH_DROP_LOGPROB
        past_audio = (
            end is not None
            and audio_duration_s is not None
            and float(end) > audio_duration_s + SEGMENT_END_SLACK_S
        )
        is_credit = is_hallucination(seg_text)
        # A credit (or a year-LESS subtitle-credit PREFIX like "Danske tekster
        # af …") whose OWN char-rate is humanly impossible is a hallucination
        # needing NO no_speech signal — two independent signals (credit shape AND
        # impossible rate) never co-occur on real speech. Catches BOTH the
        # moderate-no_speech repro the credit_pattern gate (no_speech >=
        # NO_SPEECH_DROP) lets through, AND year-less credits that is_hallucination
        # deliberately won't match on text alone (the prefix also matches real
        # dictation, so it is ONLY ever dropped together with impossible rate).
        seg_duration = (
            float(end) - float(start)
            if end is not None and start is not None
            else None
        )
        credit_rate = (
            (is_credit or _looks_like_credit_prefix(seg_text))
            and seg_duration is not None
            and seg_duration > 0  # ignore zero/inverted timestamps (bad timing data)
            and _speech_rate_exceeded(seg_text, seg_duration)
        )
        reason = None
        if likely_silence:
            reason = "no_speech+logprob"
        elif past_audio:
            reason = "end_past_audio"
        elif is_credit and no_speech >= NO_SPEECH_DROP:
            reason = "credit_pattern"
        elif credit_rate:
            reason = "credit_rate"
        if reason is not None:
            try:
                segment._drop_reason = reason
            except (AttributeError, TypeError):
                pass
            dropped.append(segment)
        else:
            kept.append(segment)
    return kept, dropped


def _speech_rate_exceeded(text: str, duration_s: float) -> bool:
    """Pure predicate: True when ``text`` packs more chars/second than humanly
    plausible.

    Real speech runs ~15-25 chars/s; the default 30 cap leaves headroom. The
    classic credit hallucination (60 chars from a 0.31 s tap = ~193 chars/s) is
    far above any real rate. ``MAX_CHARS_PER_SECOND`` of 0 disables the gate.
    Side-effect-free, so it is safe to call per-segment inside
    ``_drop_hallucinated_segments``; ``_exceeds_speech_rate`` wraps it with the
    diagnostic log line for the whole-text path.
    """
    if MAX_CHARS_PER_SECOND <= 0:
        return False
    # Count visible chars only — leading/trailing whitespace (Whisper emits a
    # leading space on segment boundaries) is not "speech" and would inflate the
    # rate slightly.
    return len(text.strip()) / max(duration_s, 0.1) > MAX_CHARS_PER_SECOND


def _exceeds_speech_rate(text: str, duration_s: float) -> bool:
    """``_speech_rate_exceeded`` plus a diagnostic log line when it fires."""
    if not _speech_rate_exceeded(text, duration_s):
        return False
    chars = len(text.strip())
    rate = chars / max(duration_s, 0.1)
    print(
        f"[stt] dropped: {chars} chars in {duration_s:.1f}s = "
        f"{rate:.0f} chars/s > {MAX_CHARS_PER_SECOND:.0f} (hallucination guard)",
        flush=True,
    )
    return True


def _segment_metric(segment) -> dict[str, Any]:
    out: dict[str, Any] = {
        "start": getattr(segment, "start", None),
        "end": getattr(segment, "end", None),
        "text": getattr(segment, "text", ""),
    }
    for name in ("avg_logprob", "no_speech_prob", "compression_ratio"):
        if hasattr(segment, name):
            out[name] = getattr(segment, name)
    return out


def _transcribe_detail(model, pcm: np.ndarray, lang: str | None) -> TranscribeResult:
    # pcm: int16 mono @ 16 kHz straight from sounddevice — already the
    # rate/layout Whisper wants, so no WAV round-trip or resample. Just
    # int16 -> float32 -> boost.
    raw_audio = pcm.reshape(-1).astype(np.float32) / 32768.0
    # PRIMARY anti-hallucination defence: cut the "dead audio" tail FIRST — before
    # the speech gate, capture metrics AND decode — so all three see the same
    # trimmed buffer. Whisper fills an empty trailing region with a subtitle
    # credit, and a long dead tail also drags the mean level down (which could
    # trip the too-quiet gate on a clip that actually contains clear speech).
    # Removes only a sustained trailing run far below the clip's speech body
    # (past a 120 ms pad); a normally-voiced or softly-trailed word is preserved.
    raw_audio, trimmed_ms = _trim_trailing_silence(raw_audio)
    if trimmed_ms > 0:
        print(
            f"[cap] trimmed {trimmed_ms:.0f}ms trailing silence (anti-hallucination)",
            flush=True,
        )
    ok, gate = _looks_like_speech(raw_audio)
    if not ok:
        print(f"[gate] {gate}", flush=True)
        return TranscribeResult(text="", gate=gate)
    print(f"[gate] {gate}", flush=True)
    audio, capture_metrics = _boost_quiet_detail(raw_audio)
    dur = len(audio) / SR
    in_dbfs = 20 * np.log10(float(np.sqrt(np.mean(audio**2)) or 1e-9))
    use_context = CONTEXT_MIN_SECONDS > 0 and dur >= CONTEXT_MIN_SECONDS
    t0 = time.monotonic()
    dictionary_prompt = _dictionary_prompt_runtime(INITIAL_PROMPT)
    prompt = dictionary_prompt.prompt
    # hallucination_silence_threshold only takes effect with word timestamps, so
    # enable both together when the guard is on.
    guard_kwargs = (
        {"word_timestamps": True, "hallucination_silence_threshold": HALLUCINATION_SILENCE_S}
        if HALLUCINATION_GUARD
        else {}
    )
    # Hold TRANSCRIBE_LOCK across the whole decode (the generator is lazy, so
    # segments must be drained inside the lock too). This serializes the final
    # pass with any in-flight live preview — the final pass may wait for at most
    # one bounded preview chunk to finish, which is acceptable.
    with TRANSCRIBE_LOCK:
        segments, info = model.transcribe(
            audio,
            language=lang,
            initial_prompt=prompt,
            beam_size=BEAM_SIZE,
            temperature=TEMPERATURES,
            condition_on_previous_text=use_context,
            no_speech_threshold=0.45,
            log_prob_threshold=-1.0,
            vad_filter=True,
            vad_parameters={
                "threshold": VAD_THRESHOLD,
                "min_silence_duration_ms": VAD_MIN_SILENCE_MS,
                "speech_pad_ms": VAD_SPEECH_PAD_MS,
            },
            **guard_kwargs,
        )
        segment_list = list(segments)
    segment_list, dropped_segments = _drop_hallucinated_segments(segment_list, dur)
    for segment in dropped_segments:
        print(
            f"[stt] dropped hallucinated segment: "
            f"reason={getattr(segment, '_drop_reason', '?')} "
            f"no_speech={float(getattr(segment, 'no_speech_prob', 0.0) or 0.0):.2f} "
            f"logprob={float(getattr(segment, 'avg_logprob', 0.0) or 0.0):.2f} "
            f"end={float(getattr(segment, 'end', 0.0) or 0.0):.1f}s "
            f"text={getattr(segment, 'text', '')!r}",
            flush=True,
        )
    # Concatenate with Whisper's OWN spacing. Each segment text already
    # carries a leading space on word boundaries (BPE tokens); strip()+
    # " ".join() drops that at segment joins -> "hørerdig". Join raw,
    # then collapse whitespace runs to one space.
    raw_text = re.sub(r"\s+", " ", "".join(s.text for s in segment_list)).strip()
    if raw_text and _exceeds_speech_rate(raw_text, dur):
        # Humanly impossible char rate -> hallucinated credit/caption. Drop the
        # text (and its segments); downstream this surfaces as an empty result
        # (reason="empty" -> state=no_text), not as an is_hallucination match.
        raw_text = ""
        segment_list = []
    dictionary_text = (
        _apply_cached_dictionary_runtime(raw_text, INITIAL_PROMPT)
        or _dictionary_runtime(raw_text, INITIAL_PROMPT)
    )
    text = dictionary_text.text
    replacements = dictionary_text.changes
    compute_s = time.monotonic() - t0
    rtf = compute_s / dur if dur > 0 else None
    seg_metrics = [_segment_metric(s) for s in segment_list]
    print(f"[stt] dur={dur:.1f}s post-boost={in_dbfs:.0f}dBFS "
          f"compute={compute_s:.1f}s rtf={rtf:.2f} text={text!r}", flush=True)
    if STT_DEBUG:
        for i, segment in enumerate(seg_metrics, 1):
            print(f"[stt-debug] segment#{i} {segment}", flush=True)
        if replacements:
            print(f"[stt-debug] dictionary replacements={replacements}", flush=True)
    return TranscribeResult(
        text=text,
        raw_text=raw_text,
        duration_s=dur,
        post_boost_dbfs=in_dbfs,
        raw_dbfs=capture_metrics.raw_dbfs,
        peak=capture_metrics.peak,
        gain=capture_metrics.gain,
        noise_dbfs=capture_metrics.noise_dbfs,
        snr_db=capture_metrics.snr_db,
        input_status=capture_metrics.input_status,
        compute_s=compute_s,
        real_time_factor=rtf,
        language=getattr(info, "language", None),
        language_probability=getattr(info, "language_probability", None),
        segments=seg_metrics,
        gate=gate,
        dictionary_terms=dictionary_prompt.terms,
        dictionary_replacements=replacements,
    )


def _transcribe(model, pcm: np.ndarray, lang: str | None) -> str:
    return _transcribe_detail(model, pcm, lang).text


def transcribe_preview(model, pcm: np.ndarray, lang: str | None) -> str:
    """Fast, display-only transcription of an in-progress recording buffer.

    Used by the live-preview thread (vp_preview) to show the sentence growing
    while the user is still holding the key. It is deliberately CHEAP and never
    touches the quality knobs the final pass uses:

      * beam_size=1, temperature=0.0, condition_on_previous_text=False — greedy,
        no fallback ladder, no cross-segment context, so it is as fast as
        possible and never queues.
      * the existing VAD settings are reused (read-only) so silence handling
        matches the final pass closely enough for a preview.
      * the hallucination guard / word-timestamps are skipped (extra compute we
        don't want for a throwaway preview).

    This must NOT mutate any shared decode state (it only reads module globals)
    and the final pass is the single source of truth for the injected text.

    Returns the (whitespace-collapsed) preview text, or "" on no/empty speech.
    The caller is responsible for acquiring TRANSCRIBE_LOCK (non-blocking) so a
    preview never runs concurrently with the final pass or another preview.
    """
    audio = pcm.reshape(-1).astype(np.float32) / 32768.0
    segments, _info = model.transcribe(
        audio,
        language=lang,
        beam_size=1,
        temperature=0.0,
        condition_on_previous_text=False,
        no_speech_threshold=0.45,
        log_prob_threshold=-1.0,
        vad_filter=True,
        vad_parameters={
            "threshold": VAD_THRESHOLD,
            "min_silence_duration_ms": VAD_MIN_SILENCE_MS,
            "speech_pad_ms": VAD_SPEECH_PAD_MS,
        },
    )
    raw_text = re.sub(r"\s+", " ", "".join(s.text for s in segments)).strip()
    return raw_text
