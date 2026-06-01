"""Optional NVIDIA Parakeet STT adapter.

This module intentionally has no NeMo imports at module import time. The
dependencies are large and optional; only VOICEPI_STT_BACKEND=parakeet should
need them.
"""
from __future__ import annotations

import contextlib
import os
import tempfile
import warnings
import wave
from dataclasses import dataclass
from typing import Any

import numpy as np

SR = 16000
DEFAULT_MODEL = "nvidia/parakeet-tdt-0.6b-v3"
PARAKEET_MODELS = [
    DEFAULT_MODEL,
    "nvidia/parakeet-tdt-1.1b",
    "nvidia/parakeet-tdt-0.6b-v2",
]
WHISPER_DEFAULT_MODEL = "large-v3-turbo"


def _debug_enabled() -> bool:
    return (os.environ.get("VOICEPI_STT_DEBUG") or "").strip().lower() not in (
        "", "0", "false", "no", "off")


@contextlib.contextmanager
def _nemo_output_context():
    if _debug_enabled():
        yield
        return
    with open(os.devnull, "w", encoding="utf-8") as sink:
        with contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
            yield


def resolve_parakeet_model_name(model_name: str | None = None) -> str:
    explicit = os.environ.get("VOICEPI_PARAKEET_MODEL")
    if explicit:
        return explicit
    if model_name and ("/" in model_name or "parakeet" in model_name.lower()):
        return model_name
    return DEFAULT_MODEL


@dataclass
class ParakeetSegment:
    text: str
    start: float | None = None
    end: float | None = None


@dataclass
class ParakeetInfo:
    language: str | None = None
    language_probability: float | None = None


def _missing_deps_error() -> RuntimeError:
    return RuntimeError(
        "VOICEPI_STT_BACKEND=parakeet requires NVIDIA NeMo ASR dependencies. "
        "Install the optional Parakeet bundle, for example: "
        "python -m pip install -r requirements-parakeet.txt. "
        "If your PyTorch/CUDA version needs a specific wheel index, install "
        "torch and torchaudio first from https://pytorch.org/get-started/locally/."
    )


def _cuda_torch_error() -> RuntimeError:
    return RuntimeError(
        "VOICEPI_STT_BACKEND=parakeet with --device cuda requires a "
        "CUDA-enabled PyTorch wheel, but the installed torch build is CPU-only. "
        "Run whisper-dictate install again after installing the latest whisper-dictate, or "
        "install manually: python -m pip install --upgrade --force-reinstall --no-deps "
        "torch==2.11.0+cu126 torchaudio==2.11.0+cu126 "
        "--index-url https://download.pytorch.org/whl/cu126"
    )


def _torch_cuda_available() -> bool:
    try:
        import torch
        return bool(torch.cuda.is_available())
    except Exception:
        return False


def _write_wav(audio: np.ndarray) -> str:
    fd, path = tempfile.mkstemp(prefix="voicepi-parakeet-", suffix=".wav")
    os.close(fd)
    clipped = np.clip(audio, -1.0, 1.0)
    pcm = (clipped * 32767.0).astype(np.int16)
    with wave.open(path, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SR)
        wav.writeframes(pcm.tobytes())
    return path


def _text_from_item(item: Any) -> str:
    if isinstance(item, str):
        return item
    for attr in ("text", "transcript"):
        value = getattr(item, attr, None)
        if isinstance(value, str):
            return value
    return str(item)


class ParakeetModel:
    def __init__(self, model_name: str | None = None, *,
                 device: str = "auto", compute_type: str = ""):
        warnings.filterwarnings(
            "ignore",
            message="Couldn't find ffmpeg or avconv.*",
            category=RuntimeWarning,
            module="pydub.utils",
        )
        try:
            import nemo.collections.asr as nemo_asr
        except ImportError as exc:
            raise _missing_deps_error() from exc

        self.model_name = resolve_parakeet_model_name(model_name)
        self.device = device
        self.compute_type = compute_type
        if device == "cuda" and not _torch_cuda_available():
            raise _cuda_torch_error()
        with _nemo_output_context():
            self._model = nemo_asr.models.ASRModel.from_pretrained(
                model_name=self.model_name)
            if device in ("cuda", "cpu") and hasattr(self._model, "to"):
                self._model.to(device)
            for method in ("eval", "freeze"):
                fn = getattr(self._model, method, None)
                if callable(fn):
                    fn()

    def _call_transcribe(self, path: str):
        try:
            return self._model.transcribe([path], batch_size=1)
        except TypeError:
            return self._model.transcribe(
                paths2audio_files=[path],
                batch_size=1,
                return_hypotheses=False,
            )

    def transcribe(self, audio: np.ndarray, **_: Any):
        path = _write_wav(audio.reshape(-1).astype(np.float32))
        try:
            with _nemo_output_context():
                result = self._call_transcribe(path)
        finally:
            try:
                os.remove(path)
            except OSError:
                pass
        if isinstance(result, tuple):
            result = result[0]
        if not isinstance(result, (list, tuple)):
            result = [result]
        text = " ".join(_text_from_item(item).strip()
                        for item in result if _text_from_item(item).strip())
        return [ParakeetSegment(text=text)], ParakeetInfo()
