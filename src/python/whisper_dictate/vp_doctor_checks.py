"""Cross-platform readiness probes for ``whisper-dictate doctor``.

The legacy doctor only covered Linux/Wayland injection; these probes make
``--doctor`` a comprehensive, platform-agnostic readiness report: app version,
config validity, the Rust helper, the configured STT backend and its
prerequisites (faster-whisper/CUDA + model cache, or the cloud API key and
reachability), the audio stack, GPU details and free disk.

Heavy deps (numpy, faster-whisper, ctranslate2, sounddevice) are imported
*inside* the probes so ``--help`` stays instant; ``--doctor`` pays the import
cost only when actually run. Network/GPU/disk probes are best-effort and report
at WARN level (``required=False``) so a healthy machine that is merely offline
or CPU-only never reports a hard FAIL.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

from whisper_dictate.vp_config import get_value


@dataclass
class Check:
    name: str
    ok: bool
    detail: str
    required: bool = True


def _import_ok(name: str) -> bool:
    try:
        __import__(name)
        return True
    except Exception:
        return False


def configured_backend() -> str:
    """The STT backend per config, normalising the faster-whisper alias."""
    backend = (get_value("VOICEPI_STT_BACKEND", "whisper") or "whisper").strip().lower()
    return "whisper" if backend in ("", "faster-whisper") else backend


def check_version() -> Check:
    from whisper_dictate.runtime import get_version

    version = get_version()
    return Check("version", version not in ("", "unknown"), version or "unknown", required=False)


def check_config() -> Check:
    path = get_value("VOICEPI_CONFIG", "")
    if not path:
        return Check("config", True, "no config file set (env defaults)", required=False)
    file = Path(path)
    if not file.exists():
        return Check("config", True, f"{path} (absent; env defaults)", required=False)
    try:
        json.loads(file.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        return Check("config", False, f"{path} (invalid: {exc})")
    return Check("config", True, f"{path} (valid JSON)")


def check_rust_helper() -> Check:
    from whisper_dictate.vp_rust import _rust_helper

    helper = _rust_helper()
    return Check(
        "rust helper",
        bool(helper),
        helper or "not found (format/dictionary/cloud helper unavailable)",
    )


def check_backend() -> Check:
    backend = configured_backend()
    # Wave 8 of #348 dropped the Parakeet backend; the doctor's accepted set
    # collapses to whisper + openai to match runtime/vp_transcribe.
    valid = backend in ("whisper", "openai")
    # An invalid backend means the app can't transcribe at all → hard FAIL.
    detail = backend if valid else f"{backend} (invalid; expected whisper/openai)"
    return Check("stt backend", valid, detail)


def audio_checks() -> list[Check]:
    checks = [Check("numpy", _import_ok("numpy"), "import numpy")]
    if not _import_ok("sounddevice"):
        checks.append(Check("sounddevice", False, "import sounddevice (audio capture)"))
        return checks
    checks.append(Check("sounddevice", True, "import sounddevice"))
    try:
        import sounddevice as sd

        inputs = [d for d in sd.query_devices() if d.get("max_input_channels", 0) > 0]
        checks.append(Check("audio input", bool(inputs), f"{len(inputs)} input device(s)"))
    except Exception as exc:  # noqa: BLE001 - device probe is best-effort
        checks.append(Check("audio input", False, f"query failed: {exc}", required=False))
    return checks


def backend_checks(backend: str, *, ping: bool = True) -> list[Check]:
    if backend == "openai":
        return _cloud_checks(ping=ping)
    # Wave 8 of #348 removed the Parakeet/NeMo branch here together with the
    # backend.
    if backend != "whisper":
        return []  # unknown backend is already flagged by check_backend
    # whisper / faster-whisper (the default local backend)
    checks = [
        Check("faster_whisper", _import_ok("faster_whisper"), "import faster_whisper"),
        Check("ctranslate2", _import_ok("ctranslate2"), "import ctranslate2"),
    ]
    checks.extend(gpu_checks())
    checks.append(model_cache_check())
    return checks


def gpu_checks() -> list[Check]:
    count = 0
    if _import_ok("ctranslate2"):
        try:
            import ctranslate2

            count = ctranslate2.get_cuda_device_count()
        except Exception:  # noqa: BLE001 - GPU probe is best-effort
            count = 0
    detail = f"{count} CUDA device(s)" if count else "0 CUDA devices (CPU only)"
    checks = [Check("cuda", count > 0, detail, required=False)]
    name = _nvidia_smi_summary()
    if name:
        checks.append(Check("gpu", True, name, required=False))
    return checks


def _nvidia_smi_summary() -> str:
    smi = shutil.which("nvidia-smi")
    if not smi:
        return ""
    try:
        out = subprocess.run(
            [smi, "--query-gpu=name,memory.total", "--format=csv,noheader"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=4,
        )
    except Exception:  # noqa: BLE001 - nvidia-smi is optional
        return ""
    if out.returncode != 0 or not out.stdout.strip():
        return ""
    return out.stdout.strip().splitlines()[0].strip()


def _cloud_checks(*, ping: bool) -> list[Check]:
    base_url = (get_value("VOICEPI_STT_BASE_URL", "https://api.openai.com/v1") or "").rstrip("/")
    key = (
        get_value("VOICEPI_STT_API_KEY", "")
        or get_value("OPENAI_API_KEY", "")
        or get_value("GROQ_API_KEY", "")
    )
    checks = [
        Check("cloud base url", bool(base_url), base_url or "unset", required=False),
        Check("cloud api key", bool(key), "present" if key else "missing (set VOICEPI_STT_API_KEY)"),
    ]
    if ping and base_url and key:
        checks.append(_cloud_reachable(base_url, key))
    return checks


_CLOUD_REACHABLE = "cloud reachable"
_MODEL_CACHE = "model cache"


def _cloud_reachable(base_url: str, key: str) -> Check:
    import urllib.error
    import urllib.request

    request = urllib.request.Request(
        f"{base_url}/models", headers={"Authorization": f"Bearer {key}"}
    )
    try:
        with urllib.request.urlopen(request, timeout=4) as resp:
            code = resp.status
    except urllib.error.HTTPError as exc:
        code = exc.code
    # Offline / network errors are a WARN, not a FAIL.
    except Exception as exc:  # noqa: BLE001
        return Check(_CLOUD_REACHABLE, False, f"unreachable: {exc}", required=False)
    if code == 200:
        return Check(_CLOUD_REACHABLE, True, f"{base_url}/models -> HTTP 200", required=False)
    # Reachable, but the key/endpoint was rejected — some providers gate /models.
    return Check(
        _CLOUD_REACHABLE,
        False,
        f"reachable but HTTP {code} (check key/permissions)",
        required=False,
    )


def _model_cache_dir() -> Path:
    hf_home = os.environ.get("HF_HOME")
    if hf_home:
        return Path(hf_home)
    return Path(os.path.expanduser("~")) / ".cache" / "huggingface"


def model_cache_check() -> Check:
    cache = _model_cache_dir()
    if not cache.exists():
        return Check(_MODEL_CACHE, True, f"{cache} (empty; downloads on first run)", required=False)
    try:
        # Sum only the model weight blobs (hub/models--*/blobs/*) rather than
        # walking the whole cache, so this stays fast even with many models.
        size = sum(f.stat().st_size for f in cache.glob("hub/models--*/blobs/*") if f.is_file())
    except OSError as exc:
        return Check(_MODEL_CACHE, True, f"{cache} (size unknown: {exc})", required=False)
    if size:
        return Check(_MODEL_CACHE, True, f"{cache} ({size / 1e9:.1f} GB in model blobs)", required=False)
    return Check(_MODEL_CACHE, True, f"{cache} (present; no model blobs yet)", required=False)


def disk_check() -> Check:
    target = _model_cache_dir()
    while not target.exists() and target != target.parent:
        target = target.parent
    try:
        free_gb = shutil.disk_usage(target).free / 1e9
    except OSError as exc:
        return Check("disk free", True, f"unknown: {exc}", required=False)
    return Check("disk free", free_gb > 2.0, f"{free_gb:.1f} GB free at {target}", required=False)


def readiness_checks(*, ping: bool = True) -> list[Check]:
    """Cross-platform readiness checks, ordered for a readable report."""
    backend = configured_backend()
    checks = [check_version(), check_config(), check_rust_helper(), check_backend()]
    checks.extend(audio_checks())
    checks.extend(backend_checks(backend, ping=ping))
    checks.append(disk_check())
    return checks
