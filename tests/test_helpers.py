from __future__ import annotations

import importlib
import io
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
import wave
import dataclasses
from contextlib import redirect_stderr, contextmanager
from pathlib import Path
from unittest.mock import patch

SRC = Path(__file__).resolve().parents[1] / "src" / "python"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

_TEST_CONFIG = os.path.join(tempfile.gettempdir(), "whisper-dictate-test-config.json")
os.environ.setdefault("VOICEPI_CONFIG", _TEST_CONFIG)
try:
    os.remove(_TEST_CONFIG)
except OSError:
    pass

try:
    _REAL_NUMPY = importlib.import_module("numpy")
except ImportError:
    _REAL_NUMPY = None


def load_voice_pi(cuda_devices: int = 0):
    for name in ("voice_pi", "whisper_dictate.runtime",
                 "whisper_dictate.vp_audio", "whisper_dictate.vp_inject",
                 "whisper_dictate.vp_cli", "whisper_dictate.vp_transcribe",
                 "whisper_dictate.vp_parakeet",
                 "whisper_dictate.vp_config",
                 "whisper_dictate.vp_postprocess",
                 "ctranslate2", "faster_whisper", "numpy",
                 "sounddevice", "pynput", "pynput.keyboard"):
        sys.modules.pop(name, None)

    ctranslate2 = types.ModuleType("ctranslate2")
    ctranslate2.get_cuda_device_count = lambda: cuda_devices
    sys.modules["ctranslate2"] = ctranslate2

    faster_whisper = types.ModuleType("faster_whisper")
    faster_whisper.WhisperModel = object
    sys.modules["faster_whisper"] = faster_whisper

    sys.modules["numpy"] = types.ModuleType("numpy")
    sys.modules["sounddevice"] = types.ModuleType("sounddevice")

    pynput = types.ModuleType("pynput")
    keyboard = types.ModuleType("keyboard")
    keyboard.Controller = object
    keyboard.Key = types.SimpleNamespace(
        ctrl_l=object(), ctrl_r=object(),
        shift_l=object(), shift_r=object(),
        alt_l=object(), alt_r=object(),
        esc=object(),
    )
    keyboard.Listener = object
    pynput.keyboard = keyboard
    sys.modules["pynput"] = pynput
    sys.modules["pynput.keyboard"] = keyboard

    return importlib.import_module("whisper_dictate.runtime")


def load_voice_pi_realnp():
    """Import voice_pi with the REAL numpy (for audio-DSP tests) but the
    heavy/uninstalled deps stubbed. CI installs numpy (see tests workflow)."""
    for name in ("voice_pi", "whisper_dictate.runtime",
                 "whisper_dictate.vp_audio", "whisper_dictate.vp_inject",
                 "whisper_dictate.vp_cli", "whisper_dictate.vp_transcribe",
                 "whisper_dictate.vp_parakeet",
                 "whisper_dictate.vp_config",
                 "whisper_dictate.vp_postprocess",
                 "ctranslate2", "faster_whisper",
                 "sounddevice", "pynput", "pynput.keyboard"):
        sys.modules.pop(name, None)
    numpy = real_numpy()
    sys.modules["numpy"] = numpy

    ct = types.ModuleType("ctranslate2")
    ct.get_cuda_device_count = lambda: 0
    sys.modules["ctranslate2"] = ct
    fw = types.ModuleType("faster_whisper")
    fw.WhisperModel = object
    sys.modules["faster_whisper"] = fw
    sys.modules["sounddevice"] = types.ModuleType("sounddevice")
    pynput = types.ModuleType("pynput")
    kb = types.ModuleType("keyboard")
    kb.Controller = object
    kb.Key = types.SimpleNamespace(
        ctrl_l=object(), ctrl_r=object(), shift_l=object(),
        shift_r=object(), alt_l=object(), alt_r=object(), esc=object())
    kb.Listener = object
    pynput.keyboard = kb
    sys.modules["pynput"] = pynput
    sys.modules["pynput.keyboard"] = kb
    return importlib.import_module("whisper_dictate.runtime")


@contextmanager
def _capture_stdout():
    import contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        yield buf


@contextmanager
def _env(**kwargs):
    old = {k: os.environ.get(k) for k in kwargs}
    for k, v in kwargs.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v
    try:
        yield
    finally:
        for k, v in old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def real_numpy():
    if _REAL_NUMPY is None:
        raise ImportError("real numpy unavailable")
    sys.modules["numpy"] = _REAL_NUMPY
    return _REAL_NUMPY

__all__ = [name for name in globals() if not name.startswith("__")]
