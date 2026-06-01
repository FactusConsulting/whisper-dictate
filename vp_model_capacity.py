"""Local GPU/VRAM capacity checks and model-fit guidance."""
from __future__ import annotations

import csv
import io
import json
import subprocess
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class GpuInfo:
    index: int
    name: str
    total_mb: int
    free_mb: int
    source: str = "nvidia-smi"

    def as_dict(self) -> dict[str, Any]:
        return {
            "index": self.index,
            "name": self.name,
            "total_mb": self.total_mb,
            "free_mb": self.free_mb,
            "source": self.source,
        }


@dataclass(frozen=True)
class ModelProfile:
    name: str
    category: str
    required_free_mb: int
    setting_hint: str
    note: str


@dataclass(frozen=True)
class ModelFit:
    profile: ModelProfile
    status: str
    detail: str

    def as_dict(self) -> dict[str, Any]:
        return {
            "name": self.profile.name,
            "category": self.profile.category,
            "required_free_mb": self.profile.required_free_mb,
            "status": self.status,
            "setting_hint": self.profile.setting_hint,
            "detail": self.detail,
            "note": self.profile.note,
        }


MODEL_PROFILES = (
    ModelProfile(
        "Whisper large-v3-turbo",
        "stt",
        1800,
        "VOICEPI_STT_BACKEND=whisper; VOICEPI_MODEL=large-v3-turbo; VOICEPI_COMPUTE_TYPE=int8_float16",
        "Fast default; best fit for small GPUs and CPU fallback.",
    ),
    ModelProfile(
        "Whisper large-v3 quantized",
        "stt",
        3200,
        "VOICEPI_STT_BACKEND=whisper; VOICEPI_MODEL=large-v3; VOICEPI_COMPUTE_TYPE=int8_float16",
        "Full Whisper model with quantized GPU compute.",
    ),
    ModelProfile(
        "Whisper large-v3 float16",
        "stt",
        5000,
        "VOICEPI_STT_BACKEND=whisper; VOICEPI_MODEL=large-v3; VOICEPI_COMPUTE_TYPE=float16",
        "Higher-quality Whisper path for GPUs with enough headroom.",
    ),
    ModelProfile(
        "Whisper large-v3 float16 high beam",
        "stt",
        8000,
        "VOICEPI_MODEL=large-v3; VOICEPI_COMPUTE_TYPE=float16; VOICEPI_BEAM_SIZE=10",
        "Useful for hard audio; beam past 16 has diminishing returns.",
    ),
    ModelProfile(
        "NVIDIA Parakeet 0.6B v3",
        "stt",
        7000,
        "VOICEPI_STT_BACKEND=parakeet; VOICEPI_PARAKEET_MODEL=nvidia/parakeet-tdt-0.6b-v3",
        "Very fast experimental STT; needs CUDA-enabled PyTorch.",
    ),
    ModelProfile(
        "NVIDIA Parakeet TDT 1.1B",
        "stt",
        12000,
        "VOICEPI_STT_BACKEND=parakeet; VOICEPI_PARAKEET_MODEL=nvidia/parakeet-tdt-1.1b",
        "English-heavy quality experiment; larger startup and VRAM footprint.",
    ),
    ModelProfile(
        "Ollama Qwen2.5 3B",
        "post",
        4500,
        "VOICEPI_POST_PROCESSOR=ollama; VOICEPI_POST_MODEL=qwen2.5:3b",
        "Small local text cleanup model; practical alongside STT on many GPUs.",
    ),
    ModelProfile(
        "Ollama Qwen2.5 7B Q4",
        "post",
        8000,
        "VOICEPI_POST_PROCESSOR=ollama; VOICEPI_POST_MODEL=qwen2.5:7b",
        "Better text cleanup if GPU has headroom; may spill to CPU otherwise.",
    ),
    ModelProfile(
        "Ollama Qwen2.5 14B Q4",
        "post",
        14000,
        "VOICEPI_POST_PROCESSOR=ollama; VOICEPI_POST_MODEL=qwen2.5:14b",
        "Higher-quality local rewrite; usually not for concurrent STT on small GPUs.",
    ),
)


def _parse_int_mb(value: str) -> int:
    return int(float(value.strip().replace("MiB", "").replace("MB", "")))


def _query_nvidia_smi() -> list[GpuInfo]:
    cmd = [
        "nvidia-smi",
        "--query-gpu=index,name,memory.total,memory.free",
        "--format=csv,noheader,nounits",
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=3, check=False)
    except (FileNotFoundError, OSError, subprocess.SubprocessError):
        return []
    if proc.returncode != 0:
        return []
    rows = csv.reader(io.StringIO(proc.stdout))
    out: list[GpuInfo] = []
    for row in rows:
        if len(row) < 4:
            continue
        try:
            out.append(GpuInfo(
                index=int(row[0].strip()),
                name=row[1].strip(),
                total_mb=_parse_int_mb(row[2]),
                free_mb=_parse_int_mb(row[3]),
            ))
        except (TypeError, ValueError):
            continue
    return out


def _query_torch_cuda() -> list[GpuInfo]:
    try:
        import torch
        if not torch.cuda.is_available():
            return []
        infos: list[GpuInfo] = []
        for index in range(torch.cuda.device_count()):
            free_b, total_b = torch.cuda.mem_get_info(index)
            infos.append(GpuInfo(
                index=index,
                name=torch.cuda.get_device_name(index),
                total_mb=int(total_b / 1024 / 1024),
                free_mb=int(free_b / 1024 / 1024),
                source="torch",
            ))
        return infos
    except Exception:
        return []


def query_gpus() -> list[GpuInfo]:
    """Return local NVIDIA CUDA GPUs with total/free VRAM in MB."""
    return _query_nvidia_smi() or _query_torch_cuda()


def estimate_model_fits(
    gpus: list[GpuInfo] | None = None,
    profiles: tuple[ModelProfile, ...] = MODEL_PROFILES,
) -> tuple[list[GpuInfo], list[ModelFit]]:
    gpus = query_gpus() if gpus is None else gpus
    best_total = max((gpu.total_mb for gpu in gpus), default=0)
    best_free = max((gpu.free_mb for gpu in gpus), default=0)
    fits: list[ModelFit] = []
    for profile in profiles:
        required = profile.required_free_mb
        if best_free >= required:
            status = "ok"
            detail = f"fits now; needs about {required} MB free VRAM"
        elif best_total >= required:
            status = "free-vram"
            detail = (
                f"GPU is large enough, but only {best_free} MB is free; "
                f"stop other GPU processes to reach about {required} MB"
            )
        else:
            status = "too-small"
            detail = (
                f"needs about {required} MB free VRAM; largest GPU has "
                f"{best_total} MB total"
            )
        fits.append(ModelFit(profile, status, detail))
    return gpus, fits


def capacity_report(*, as_json: bool = False) -> str:
    gpus, fits = estimate_model_fits()
    if as_json:
        return json.dumps({
            "gpus": [gpu.as_dict() for gpu in gpus],
            "models": [fit.as_dict() for fit in fits],
        }, ensure_ascii=False, sort_keys=True)

    lines: list[str] = []
    if gpus:
        lines.append("GPU capacity:")
        for gpu in gpus:
            lines.append(
                f"  [{gpu.index}] {gpu.name}: "
                f"{gpu.free_mb} MB free / {gpu.total_mb} MB total ({gpu.source})"
            )
    else:
        lines.append("GPU capacity: no NVIDIA CUDA GPU detected")
    lines.append("")
    lines.append("Local model fit:")
    for fit in fits:
        marker = {"ok": "OK", "free-vram": "FREE VRAM", "too-small": "NO"}[fit.status]
        lines.append(
            f"  {marker:<9} {fit.profile.name:<34} "
            f"~{fit.profile.required_free_mb} MB  {fit.profile.setting_hint}"
        )
        lines.append(f"            {fit.detail}")
    lines.append("")
    lines.append("Use free VRAM for the current decision; stop whisper-dictate or other GPU apps before benchmarking.")
    return "\n".join(lines)
