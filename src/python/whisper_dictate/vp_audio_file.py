"""Audio-file decode, microphone calibration, and file transcription.

Extracted from runtime.py. Everything here works on an already-recorded buffer
or file (as opposed to the live push-to-talk loop in vp_dictate):

  * decode WAV / ffmpeg-supported files to mono 16 kHz int16,
  * record + analyse a calibration sample (``--calibrate-mic`` / ``--calibrate-file``),
  * transcribe a file end to end (``--transcribe-file``).

numpy / sounddevice / SR and the transcribe/postprocess helpers are imported
lazily inside the functions so ``--help`` / ``--doctor`` stay free of heavy deps.
"""
from __future__ import annotations

import contextlib
import json
import subprocess
import sys
import time
import wave
from pathlib import Path

from whisper_dictate.vp_events import _base_event, _compact_text
from whisper_dictate.vp_postprocess import postprocess_text


def _calibration_dbfs(audio) -> float:
    import numpy as np

    rms = float(np.sqrt(np.mean(audio.reshape(-1).astype(np.float32) ** 2)) or 1e-9)
    return 20 * np.log10(rms)


def _calibration_status(raw_dbfs: float, snr_db: float) -> tuple[str, list[str]]:
    warnings_list: list[str] = []
    if raw_dbfs < -55:
        warnings_list.append("input is very quiet")
    elif raw_dbfs < -42:
        warnings_list.append("input is quiet")
    if snr_db < 6:
        warnings_list.append("speech/noise contrast is too low")
    elif snr_db < 15:
        warnings_list.append("speech/noise contrast is marginal")
    if not warnings_list:
        return "pass", []
    if raw_dbfs < -55 or snr_db < 6:
        return "fail", warnings_list
    return "warn", warnings_list


def analyze_calibration_audio(pcm) -> dict:
    import numpy as np
    from whisper_dictate.vp_audio import _noise_snr
    from whisper_dictate.vp_transcribe import SR

    audio = pcm.reshape(-1).astype(np.float32)
    if pcm.dtype.kind in ("i", "u"):
        audio = audio / 32768.0
    raw_dbfs = _calibration_dbfs(audio)
    noise_dbfs, snr_db = _noise_snr(audio)
    peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
    status, warnings_list = _calibration_status(raw_dbfs, snr_db)
    recommended_min_input = min(-35.0, max(-65.0, raw_dbfs - 18.0))
    recommended_min_snr = 6.0 if snr_db < 15 else min(12.0, max(6.0, snr_db - 12.0))
    return {
        "event": "mic_calibration",
        "status": status,
        "warnings": warnings_list,
        "duration_s": len(audio) / SR if len(audio) else 0.0,
        "raw_dbfs": raw_dbfs,
        "noise_dbfs": noise_dbfs,
        "snr_db": snr_db,
        "peak": peak,
        "recommended": {
            "VOICEPI_TARGET_DBFS": "-20",
            "VOICEPI_MIN_INPUT_DBFS": f"{recommended_min_input:.0f}",
            "VOICEPI_MIN_SNR_DB": f"{recommended_min_snr:.0f}",
        },
    }


def record_calibration_audio(seconds: float):
    import numpy as np
    import sounddevice as sd
    from whisper_dictate.vp_transcribe import SR

    seconds = max(1.0, float(seconds))
    print(f"[calibrate] speak normally for {seconds:.1f}s...", flush=True)
    audio = sd.rec(int(seconds * SR), samplerate=SR, channels=1, dtype="int16")
    sd.wait()
    return audio.astype(np.int16)


def print_calibration_result(result: dict, *, as_json: bool = False) -> None:
    if as_json:
        print(json.dumps(result, ensure_ascii=False, separators=(",", ":")),
              flush=True)
        return
    print(f"[calibrate] status={result['status']}", flush=True)
    print(
        "[calibrate] "
        f"raw={result['raw_dbfs']:.0f}dBFS "
        f"noise={result['noise_dbfs']:.0f}dBFS "
        f"snr={result['snr_db']:.0f}dB "
        f"peak={result['peak']:.3f}",
        flush=True,
    )
    for warning in result["warnings"]:
        print(f"[calibrate] warning: {warning}", flush=True)
    rec = result["recommended"]
    print("[calibrate] recommended settings:", flush=True)
    for key, value in rec.items():
        print(f"  {key}={value}", flush=True)


def calibrate_microphone(seconds: float, *, as_json: bool = False) -> dict:
    pcm = record_calibration_audio(seconds)
    result = analyze_calibration_audio(pcm)
    print_calibration_result(result, as_json=as_json)
    return result


def calibrate_file(path: str, *, as_json: bool = False) -> dict:
    t0 = time.monotonic()
    pcm = load_audio_file(path)
    result = analyze_calibration_audio(pcm)
    result["source_file"] = path
    result["decode_s"] = time.monotonic() - t0
    print_calibration_result(result, as_json=as_json)
    return result


def _mono_float_to_int16(audio):
    import numpy as np

    audio = np.clip(audio.reshape(-1), -1.0, 1.0)
    return (audio * 32767.0).astype(np.int16).reshape(-1, 1)


def _resample_mono(audio, source_rate: int):
    import numpy as np
    from whisper_dictate.vp_transcribe import SR

    if source_rate == SR:
        return audio.astype(np.float32)
    if len(audio) == 0:
        return audio.astype(np.float32)
    duration = len(audio) / float(source_rate)
    target_len = max(1, int(round(duration * SR)))
    src_x = np.linspace(0.0, duration, num=len(audio), endpoint=False)
    dst_x = np.linspace(0.0, duration, num=target_len, endpoint=False)
    return np.interp(dst_x, src_x, audio).astype(np.float32)


def _decode_wav(path: Path):
    import numpy as np

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


def _decode_with_ffmpeg(path: Path):
    import numpy as np
    from whisper_dictate.vp_transcribe import SR

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


def load_audio_file(path: str | Path):
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
    model,
    path: str | Path,
    lang: str | None,
    *,
    model_name: str,
    stt_backend: str,
    device: str,
    compute_type: str,
) -> dict:
    from whisper_dictate.vp_transcribe import _transcribe_detail

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
        audio_raw_dbfs=result.raw_dbfs,
        audio_peak=result.peak,
        audio_gain=result.gain,
        audio_noise_dbfs=result.noise_dbfs,
        audio_snr_db=result.snr_db,
        audio_input_status=result.input_status,
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


def print_transcribe_file_result(event: dict, *, as_json: bool) -> None:
    if as_json:
        print(json.dumps(event, ensure_ascii=False, separators=(",", ":")),
              flush=True)
    else:
        print(event.get("text", ""), flush=True)
