"""Audio DSP + capture-device probing.

Verbatim move from voice_pi.py. Pure numpy DSP (no faster_whisper).
Behaviour is pinned by AudioDspTests (real numpy, CI) — this extraction
must keep those green unchanged.
"""
from __future__ import annotations

from dataclasses import dataclass
import os
import sys

import numpy as np

from whisper_dictate.vp_config import apply_config_to_environ, get_value

apply_config_to_environ()

# Target loudness (dBFS) quiet input is boosted toward before Whisper
# sees it. Soft voiced speech lands at -35..-45 dBFS where Whisper's
# no-speech gate eats it; normalising to ~-20 recovers it without
# clipping. Lower (e.g. -16) = boost harder.
TARGET_DBFS = float(get_value("VOICEPI_TARGET_DBFS", "-20") or "-20")
# Raw-input gate before gain boost. Without this, near-silence gets boosted
# into Whisper's comfort range; with a fixed language hint, Danish silence
# often decodes as a plausible short phrase such as "Tak."
MIN_INPUT_DBFS = float(get_value("VOICEPI_MIN_INPUT_DBFS", "-55") or "-55")
MIN_INPUT_SNR_DB = float(get_value("VOICEPI_MIN_SNR_DB", "6") or "6")
_ANSI_BOLD = "\033[1m"
_ANSI_RESET = "\033[0m"


@dataclass(frozen=True)
class AudioCaptureMetrics:
    raw_dbfs: float
    peak: float
    gain: float
    noise_dbfs: float
    snr_db: float
    input_status: str


def _highlight_cap_line(line: str) -> str:
    if os.environ.get("NO_COLOR") or os.environ.get("VOICEPI_NO_COLOR"):
        return line
    isatty = getattr(sys.stdout, "isatty", None)
    if not callable(isatty) or not isatty():
        return line
    return f"{_ANSI_BOLD}{line}{_ANSI_RESET}"


def _noise_snr(a: np.ndarray) -> tuple[float, float]:
    # Percentile-based noise-floor / SNR estimate — no VAD, no deps.
    # Frame the RAW (pre-boost) signal into 30 ms windows; the quiet
    # frames between/around words ARE the noise. Noise floor = 10th
    # pct of per-frame RMS (a real mic property in dBFS); SNR = how
    # far the speech (90th pct) sits above it. SNR is gain-invariant
    # so a uniform boost can't flatter it. Few-frame guard avoids
    # log10(0) on near-empty buffers.
    fr = 480  # 30 ms @ 16 kHz
    n = len(a) // fr
    if n < 4:
        return -90.0, 0.0
    frm = a[:n * fr].reshape(n, fr)
    rms = np.sqrt(np.mean(frm.astype(np.float64) ** 2, axis=1))
    lo = float(np.percentile(rms, 10)) or 1e-9
    hi = float(np.percentile(rms, 90)) or 1e-9
    noise_dbfs = 20 * np.log10(lo)
    snr_db = 20 * np.log10(hi / lo)
    return noise_dbfs, snr_db


def _input_level_status(raw_dbfs: float, peak: float, snr_db: float) -> str:
    if raw_dbfs < MIN_INPUT_DBFS:
        return "too_quiet"
    if snr_db < MIN_INPUT_SNR_DB:
        return "low_snr"
    if peak >= 0.98:
        return "clip_risk"
    if peak >= 0.75 or raw_dbfs > -18:
        return "hot"
    if raw_dbfs < -42:
        return "quiet"
    return "good"


def _capture_metrics(a: np.ndarray) -> AudioCaptureMetrics:
    rms = float(np.sqrt(np.mean(a**2)) or 1e-9)
    cur_dbfs = 20 * np.log10(rms)
    gain = 10 ** ((TARGET_DBFS - cur_dbfs) / 20)
    peak = float(np.max(np.abs(a)) or 1e-9)
    gain = min(gain, 0.99 / peak)  # never clip
    noise_dbfs, snr_db = _noise_snr(a)
    input_status = _input_level_status(cur_dbfs, peak, snr_db)
    return AudioCaptureMetrics(
        raw_dbfs=cur_dbfs,
        peak=peak,
        gain=gain,
        noise_dbfs=noise_dbfs,
        snr_db=snr_db,
        input_status=input_status,
    )


def _boost_quiet_detail(a: np.ndarray) -> tuple[np.ndarray, AudioCaptureMetrics]:
    metrics = _capture_metrics(a)
    line = (
        f"[cap] raw={metrics.raw_dbfs:.0f}dBFS peak={metrics.peak:.3f} "
        f"input={metrics.input_status} gain={metrics.gain:.1f}x "
        f"noise={metrics.noise_dbfs:.0f}dBFS snr={metrics.snr_db:.0f}dB"
    )
    print(_highlight_cap_line(line), flush=True)
    return (a * metrics.gain).astype(np.float32), metrics


def _boost_quiet(a: np.ndarray) -> np.ndarray:
    return _boost_quiet_detail(a)[0]


def compute_audio_metrics(pcm: np.ndarray) -> AudioCaptureMetrics:
    """Public wrapper: compute audio capture metrics for ``pcm`` (int16 mono).

    Converts int16 → float32 (as the transcription path does) and delegates
    to ``_capture_metrics``.  Safe to call after the recording has finished —
    used by the dictation loop to populate the ``[health]`` line when the
    transcription produced no text so the user can see whether the input was
    too quiet or too noisy.
    """
    raw_audio = pcm.reshape(-1).astype(np.float32) / 32768.0
    return _capture_metrics(raw_audio)


def _looks_like_speech(a: np.ndarray) -> tuple[bool, str]:
    rms = float(np.sqrt(np.mean(a**2)) or 1e-9)
    raw_dbfs = 20 * np.log10(rms)
    peak = float(np.max(np.abs(a)) or 1e-9)
    noise_dbfs, snr_db = _noise_snr(a)
    input_status = _input_level_status(raw_dbfs, peak, snr_db)
    if raw_dbfs < MIN_INPUT_DBFS:
        return False, (
            f"input too quiet: raw={raw_dbfs:.0f}dBFS "
            f"< {MIN_INPUT_DBFS:.0f}dBFS input={input_status}"
        )
    if snr_db < MIN_INPUT_SNR_DB:
        return False, (
            f"no speech contrast: snr={snr_db:.0f}dB "
            f"< {MIN_INPUT_SNR_DB:.0f}dB input={input_status}"
        )
    return True, (
        f"raw={raw_dbfs:.0f}dBFS noise={noise_dbfs:.0f}dBFS "
        f"snr={snr_db:.0f}dB input={input_status}"
    )


def _find_arecord_device() -> str | None:
    # On PipeWire (Ubuntu 24.04+) PortAudio opens ALSA hardware directly and
    # bypasses PipeWire's mixer — the mic reads as silence. arecord with
    # -D pipewire routes through PipeWire correctly.
    import subprocess, shutil, signal
    if not shutil.which("arecord"):
        return None
    for dev in ("pipewire", "default"):
        try:
            # Start without -d (duration), then treat "still running after
            # 0.3s" as evidence that the device opened successfully.
            p = subprocess.Popen(
                ["arecord", "-D", dev, "-f", "S16_LE", "-r", "16000", "-"],
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            try:
                p.wait(timeout=0.3)
            except subprocess.TimeoutExpired:
                pass
            p.send_signal(signal.SIGTERM)
            p.wait(timeout=2)
            stderr = p.stderr.read().decode(errors="replace")
            # SIGTERM gives "Aborted by signal Terminated" — that means it opened OK
            if "Terminated" in stderr or p.returncode in (0, -15, 15):
                return dev
        except Exception:
            pass
    return None
