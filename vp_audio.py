"""Audio DSP + capture-device probing.

Verbatim move from voice_pi.py. Pure numpy DSP (no faster_whisper).
Behaviour is pinned by AudioDspTests (real numpy, CI) — this extraction
must keep those green unchanged.
"""
from __future__ import annotations

import os
import time

import numpy as np

# Target loudness (dBFS) quiet input is boosted toward before Whisper
# sees it. Soft voiced speech lands at -35..-45 dBFS where Whisper's
# no-speech gate eats it; normalising to ~-20 recovers it without
# clipping. Lower (e.g. -16) = boost harder.
TARGET_DBFS = float(os.environ.get("VOICEPI_TARGET_DBFS", "-20"))
# Raw-input gate before gain boost. Without this, near-silence gets boosted
# into Whisper's comfort range; with a fixed language hint, Danish silence
# often decodes as a plausible short phrase such as "Tak."
MIN_INPUT_DBFS = float(os.environ.get("VOICEPI_MIN_INPUT_DBFS", "-55"))
MIN_INPUT_SNR_DB = float(os.environ.get("VOICEPI_MIN_SNR_DB", "6"))


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


def _boost_quiet(a: np.ndarray) -> np.ndarray:
    rms = float(np.sqrt(np.mean(a**2)) or 1e-9)
    cur_dbfs = 20 * np.log10(rms)
    gain = 10 ** ((TARGET_DBFS - cur_dbfs) / 20)
    peak = float(np.max(np.abs(a)) or 1e-9)
    gain = min(gain, 0.99 / peak)  # never clip
    noise_dbfs, snr_db = _noise_snr(a)
    print(f"[cap] raw={cur_dbfs:.0f}dBFS peak={peak:.3f} gain={gain:.1f}x "
          f"noise={noise_dbfs:.0f}dBFS snr={snr_db:.0f}dB", flush=True)
    return (a * gain).astype(np.float32)


def _looks_like_speech(a: np.ndarray) -> tuple[bool, str]:
    rms = float(np.sqrt(np.mean(a**2)) or 1e-9)
    raw_dbfs = 20 * np.log10(rms)
    noise_dbfs, snr_db = _noise_snr(a)
    if raw_dbfs < MIN_INPUT_DBFS:
        return False, (
            f"input too quiet: raw={raw_dbfs:.0f}dBFS "
            f"< {MIN_INPUT_DBFS:.0f}dBFS"
        )
    if snr_db < MIN_INPUT_SNR_DB:
        return False, (
            f"no speech contrast: snr={snr_db:.0f}dB "
            f"< {MIN_INPUT_SNR_DB:.0f}dB"
        )
    return True, (
        f"raw={raw_dbfs:.0f}dBFS noise={noise_dbfs:.0f}dBFS "
        f"snr={snr_db:.0f}dB"
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
            # Start without -d (duration), immediately SIGTERM after 0.3s
            p = subprocess.Popen(
                ["arecord", "-D", dev, "-f", "S16_LE", "-r", "16000", "-"],
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            time.sleep(0.3)
            p.send_signal(signal.SIGTERM)
            p.wait(timeout=2)
            stderr = p.stderr.read().decode(errors="replace")
            # SIGTERM gives "Aborted by signal Terminated" — that means it opened OK
            if "Terminated" in stderr or p.returncode in (0, -15, 15):
                return dev
        except Exception:
            pass
    return None
