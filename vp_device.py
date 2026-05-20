"""Compute-device resolution (auto/cuda/cpu).

Pure: no numpy/faster_whisper imported at module load (ctranslate2 is
imported lazily inside _resolve_device, matching the original). Verbatim
move from voice_pi.py — the existing DeviceResolutionTests /
ArgumentParserTests are the behaviour contract.

VOICEPI_COMPUTE_TYPE (optional): overrides the auto-picked compute_type.
Useful on big GPUs where the quantised int8_float16 default trades a
little accuracy for VRAM/speed — set "float16" (or "bfloat16" on
Ampere/Ada+, "float32" for maximum) to opt into a higher-precision path.
ctranslate2 validates the value when the model loads; an unsupported
value will raise there, not here.
"""
from __future__ import annotations

import os

VALID_DEVICES = ("auto", "cuda", "cpu")


def _resolve_device(want: str) -> tuple[str, str]:
    # → (device, compute_type). "auto" uses the GPU if a CUDA/NVIDIA
    # device is present, else CPU. faster-whisper/ctranslate2 only
    # accelerate on NVIDIA, so an AMD-GPU machine resolves to "cpu"
    # (same as a no-GPU box). int8_float16 on GPU, int8 on CPU —
    # both overridable via VOICEPI_COMPUTE_TYPE.
    want = (want or "auto").lower()
    if want not in VALID_DEVICES:
        raise ValueError(f"invalid device '{want}' (expected: "
                         f"{', '.join(VALID_DEVICES)})")

    override = (os.environ.get("VOICEPI_COMPUTE_TYPE") or "").strip() or None

    def _ct(default: str) -> str:
        return override if override else default

    if want == "cuda":
        return "cuda", _ct("int8_float16")
    if want == "cpu":
        return "cpu", _ct("int8")
    try:
        import ctranslate2
        if ctranslate2.get_cuda_device_count() > 0:
            return "cuda", _ct("int8_float16")
    except Exception:  # noqa: BLE001 — any failure → safe CPU fallback
        pass
    return "cpu", _ct("int8")
