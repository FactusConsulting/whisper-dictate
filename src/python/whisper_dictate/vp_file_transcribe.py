"""Audio-file transcription helpers for CLI and benchmarks."""
from __future__ import annotations

import contextlib
import json
import subprocess
import sys
import time
import wave
from pathlib import Path
from typing import Any

import numpy as np

from whisper_dictate.vp_postprocess import postprocess_text
from whisper_dictate.vp_transcribe import SR, _transcribe_detail


def _compact_text(text: str, limit: int = 240) -> str:
    text = " ".join(text.split())
    return text if len(text) <= limit else text[: limit - 3] + "..."


def _base_event(**fields: Any) -> dict[str, Any]:
    event = {"ts": time.time()}
    event.update(fields)
    return event


def _mono_float_to_int16(audio: np.ndarray) -> np.ndarray:
    audio = np.clip(audio.reshape(-1), -1.0, 1.0)
    return (audio * 32767.0).astype(np.int16).reshape(-1, 1)


def _resample_mono(audio: np.ndarray, source_rate: int) -> np.ndarray:
    if source_rate == SR:
        return audio.astype(np.float32)
    if len(audio) == 0:
        return audio.astype(np.float32)
    duration = len(audio) / float(source_rate)
    target_len = max(1, int(round(duration * SR)))
    src_x = np.linspace(0.0, duration, num=len(audio), endpoint=False)
    dst_x = np.linspace(0.0, duration, num=target_len, endpoint=False)
    return np.interp(dst_x, src_x, audio).astype(np.float32)


def _decode_wav(path: Path) -> np.ndarray:
    with wave.open(str(path), "rb") as wav:
        channels = wav.getnchannels()
        sample_width = wav.getsampwidth()
        rate = wav.getframerate()
        frames = wav.readframes(wav.getnframes())
    if sample_width != 2:
        raise ValueError(
            f"{path} uses {sample_width * 8}-bit WAV samples; only 16-bit PCM "
            "WAV is supported without ffmpeg")
    pcm = np.frombuffer(frames, dtype=np.int16)
    if channels > 1:
        pcm = pcm.reshape(-1, channels).mean(axis=1).astype(np.int16)
    audio = pcm.astype(np.float32) / 32768.0
    return _mono_float_to_int16(_resample_mono(audio, rate))


def _decode_with_ffmpeg(path: Path) -> np.ndarray:
    cmd = [
        "ffmpeg", "-v", "error", "-i", str(path),
        "-f", "s16le", "-acodec", "pcm_s16le", "-ac", "1", "-ar", str(SR), "-",
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, check=True)
    except FileNotFoundError as exc:
        raise RuntimeError(
            f"{path.suffix or 'audio'} files require ffmpeg unless they are "
            "16-bit PCM WAV. Install ffmpeg or pass a .wav file.") from exc
    except subprocess.CalledProcessError as exc:
        err = exc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"ffmpeg could not decode {path}: {err}") from exc
    pcm = np.frombuffer(proc.stdout, dtype=np.int16)
    return pcm.reshape(-1, 1)


def load_audio_file(path: str | Path) -> np.ndarray:
    p = Path(path)
    if not p.exists():
        raise FileNotFoundError(p)
    if p.suffix.lower() == ".wav":
        try:
            return _decode_wav(p)
        except (wave.Error, ValueError):
            return _decode_with_ffmpeg(p)
    return _decode_with_ffmpeg(p)


def transcribe_file_event(
    model: Any,
    path: str | Path,
    lang: str | None,
    *,
    model_name: str,
    stt_backend: str,
    device: str,
    compute_type: str,
) -> dict[str, Any]:
    p = Path(path)
    pcm = load_audio_file(p)
    with contextlib.redirect_stdout(sys.stderr):
        result = _transcribe_detail(model, pcm, lang)
        post_result = postprocess_text(result.text)
    final_text = post_result.text
    return _base_event(
        event="file_transcription",
        text=final_text,
        dictionary_text=result.text,
        raw_text=result.raw_text or result.text,
        text_preview=_compact_text(final_text),
        text_chars=len(final_text),
        recording_s=result.duration_s,
        audio_duration_s=result.duration_s,
        post_boost_dbfs=result.post_boost_dbfs,
        compute_s=result.compute_s,
        real_time_factor=result.real_time_factor,
        language=result.language or lang or "auto",
        language_probability=result.language_probability,
        gate=result.gate,
        model=model_name,
        stt_backend=stt_backend,
        device=device,
        compute_type=compute_type,
        source_file=str(p),
        segments=result.segments,
        dictionary_terms=result.dictionary_terms,
        dictionary_replacements=result.dictionary_replacements,
        post_processor=post_result.provider,
        post_mode=post_result.mode,
        post_model=post_result.model,
        post_latency_ms=post_result.latency_ms,
        post_changed=post_result.changed,
        post_fallback=post_result.fallback,
        post_error=post_result.error or None,
        post_redacted=post_result.redacted,
        post_redactions=post_result.redactions or [],
    )


def print_transcribe_file_result(event: dict[str, Any], *, as_json: bool) -> None:
    if as_json:
        print(json.dumps(event, ensure_ascii=False, separators=(",", ":")),
              flush=True)
    else:
        print(event.get("text", ""), flush=True)
