"""Privacy controls shared by CLI, UI and backend loading."""
from __future__ import annotations

import os

from vp_config import apply_config_to_environ, get_value

apply_config_to_environ()

LOCAL_BACKENDS = frozenset({"whisper", "faster-whisper", "parakeet"})
LOCAL_PROCESSORS = frozenset({"none", "ollama"})
_OFFLINE_ENV = (
    "HF_HUB_OFFLINE",
    "TRANSFORMERS_OFFLINE",
    "HF_DATASETS_OFFLINE",
    "HF_HUB_DISABLE_TELEMETRY",
)


def truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def local_only_enabled() -> bool:
    return truthy(get_value("VOICEPI_LOCAL_ONLY"))


def apply_local_only_network_lock() -> bool:
    """Best-effort local-only guard.

    This does not install an OS firewall rule. It tells the model/provider
    libraries we use to stay offline, and backend validation rejects cloud
    providers before they can run.
    """
    if not local_only_enabled():
        return False
    for name in _OFFLINE_ENV:
        os.environ.setdefault(name, "1")
    os.environ.setdefault("WANDB_DISABLED", "true")
    os.environ.setdefault("WANDB_MODE", "offline")
    return True


def assert_local_backend(backend: str, *, feature: str = "STT") -> None:
    if not local_only_enabled():
        return
    normalized = (backend or "").strip().lower()
    if normalized not in LOCAL_BACKENDS:
        raise RuntimeError(
            f"VOICEPI_LOCAL_ONLY=1 blocks {feature} backend {backend!r}; "
            "choose a local backend or disable local-only mode.")


def assert_local_processor(processor: str) -> None:
    if not local_only_enabled():
        return
    normalized = (processor or "").strip().lower()
    if normalized not in LOCAL_PROCESSORS:
        raise RuntimeError(
            f"VOICEPI_LOCAL_ONLY=1 blocks post-processing provider {processor!r}; "
            "choose a local provider or disable local-only mode.")
