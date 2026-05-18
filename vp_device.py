"""Compute-device resolution (auto/cuda/cpu).

Pure: no numpy/faster_whisper imported at module load (ctranslate2 is
imported lazily inside _resolve_device, matching the original). Verbatim
move from voice_pi.py — the existing DeviceResolutionTests /
ArgumentParserTests are the behaviour contract.
"""
from __future__ import annotations

VALID_DEVICES = ("auto", "cuda", "cpu")


def _resolve_device(want: str) -> tuple[str, str]:
    # → (device, compute_type). "auto" uses the GPU if a CUDA/NVIDIA
    # device is present, else CPU. faster-whisper/ctranslate2 only
    # accelerate on NVIDIA, so an AMD-GPU machine resolves to "cpu"
    # (same as a no-GPU box). int8_float16 on GPU, int8 on CPU.
    want = (want or "auto").lower()
    if want not in VALID_DEVICES:
        raise ValueError(f"invalid device '{want}' (expected: "
                         f"{', '.join(VALID_DEVICES)})")
    if want == "cuda":
        return "cuda", "int8_float16"
    if want == "cpu":
        return "cpu", "int8"
    try:
        import ctranslate2
        if ctranslate2.get_cuda_device_count() > 0:
            return "cuda", "int8_float16"
    except Exception:  # noqa: BLE001 — any failure → safe CPU fallback
        pass
    return "cpu", "int8"
