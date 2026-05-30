"""Microphone calibration analysis and CLI helpers."""
from __future__ import annotations

import json
import time
from typing import Any

import numpy as np

from vp_audio import _noise_snr
from vp_transcribe import SR


def _dbfs(audio: np.ndarray) -> float:
    rms = float(np.sqrt(np.mean(audio.reshape(-1).astype(np.float32) ** 2)) or 1e-9)
    return 20 * np.log10(rms)


def _status(raw_dbfs: float, snr_db: float) -> tuple[str, list[str]]:
    warnings: list[str] = []
    if raw_dbfs < -55:
        warnings.append("input is very quiet")
    elif raw_dbfs < -42:
        warnings.append("input is quiet")
    if snr_db < 6:
        warnings.append("speech/noise contrast is too low")
    elif snr_db < 15:
        warnings.append("speech/noise contrast is marginal")
    if not warnings:
        return "pass", []
    if raw_dbfs < -55 or snr_db < 6:
        return "fail", warnings
    return "warn", warnings


def analyze_calibration_audio(pcm: np.ndarray) -> dict[str, Any]:
    audio = pcm.reshape(-1).astype(np.float32)
    if pcm.dtype.kind in ("i", "u"):
        audio = audio / 32768.0
    raw_dbfs = _dbfs(audio)
    noise_dbfs, snr_db = _noise_snr(audio)
    peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
    status, warnings = _status(raw_dbfs, snr_db)
    recommended_min_input = min(-35.0, max(-65.0, raw_dbfs - 18.0))
    recommended_min_snr = 6.0 if snr_db < 15 else min(12.0, max(6.0, snr_db - 12.0))
    return {
        "event": "mic_calibration",
        "status": status,
        "warnings": warnings,
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


def record_calibration_audio(seconds: float) -> np.ndarray:
    import sounddevice as sd

    seconds = max(1.0, float(seconds))
    print(f"[calibrate] speak normally for {seconds:.1f}s...", flush=True)
    audio = sd.rec(int(seconds * SR), samplerate=SR, channels=1, dtype="int16")
    sd.wait()
    return audio.astype(np.int16)


def print_calibration_result(result: dict[str, Any], *, as_json: bool = False) -> None:
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


def calibrate_microphone(seconds: float, *, as_json: bool = False) -> dict[str, Any]:
    pcm = record_calibration_audio(seconds)
    result = analyze_calibration_audio(pcm)
    print_calibration_result(result, as_json=as_json)
    return result


def calibrate_file(path: str, *, as_json: bool = False) -> dict[str, Any]:
    from vp_file_transcribe import load_audio_file

    t0 = time.monotonic()
    pcm = load_audio_file(path)
    result = analyze_calibration_audio(pcm)
    result["source_file"] = path
    result["decode_s"] = time.monotonic() - t0
    print_calibration_result(result, as_json=as_json)
    return result
