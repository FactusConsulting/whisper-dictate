"""Engine / STT-implementation provenance for the utterance record.

The Python counterpart of ``src/rust/dictate/provenance.rs`` -- the two
files MUST agree, because both write to the same history / metrics JSONL
schema and a consumer cannot tell which produced a given row.

WHY THIS EXISTS
---------------
An utterance record used to carry only the *configured* stack::

    {"compute_type":"int8_float16","real_time_factor":0.23,"compute_ms":351,
     "model":"large-v3-turbo","stt_backend":"whisper","device":"auto"}

Every one of those fields is emitted by BOTH this worker and the Rust
in-process session, and ``stt_backend`` names the backend the user
*selected* (``whisper`` / ``openai``), not the code that ran. So the row
could not distinguish

* the Rust in-process session running whisper.cpp on a Vulkan GPU, from
* this worker running faster-whisper/CTranslate2 on CUDA, from
* this worker shelling out to the Rust ``transcribe-server`` helper, from
* any of those having silently fallen back to CPU.

Three fields close that, and this module owns their vocabulary:

``engine``
    Which runtime served the utterance (:data:`ENGINE_PYTHON_WORKER` here).
``stt_impl``
    Which transcription implementation actually ran.
``stt_accel``
    Which compute path it actually used -- read from the LOADED MODEL, not
    from the ``device`` setting (which is typically ``auto`` and says
    nothing about the outcome).

All labels are lowercase ASCII: they land on the console via the startup
line and must survive cmd.exe / PowerShell code pages.
"""
from __future__ import annotations

from typing import Any

# --- engine -----------------------------------------------------------

#: This worker: ``python -m whisper_dictate.runtime``.
ENGINE_PYTHON_WORKER = "python-worker"
#: The Rust in-process dictation session. Not produced here; defined so
#: the two possible values live in one place and tests can pin both.
ENGINE_RUST_IN_PROCESS = "rust-in-process"

# --- stt_impl ---------------------------------------------------------

#: whisper.cpp, via the Rust ``transcribe-wav`` / ``transcribe-server``
#: helper (``VOICEPI_TRANSCRIBE_BACKEND=rust``).
STT_IMPL_WHISPER_CPP = "whisper.cpp"
#: In-process faster-whisper / CTranslate2 bindings (the default).
STT_IMPL_FASTER_WHISPER = "faster-whisper"
#: An OpenAI ``/audio/transcriptions`` endpoint.
STT_IMPL_CLOUD_OPENAI = "cloud-openai"
#: Groq's OpenAI-compatible endpoint. Separate from OpenAI because
#: ``stt_backend`` spells both ``openai`` -- only the base URL tells them
#: apart, and they are different services with different failure modes.
STT_IMPL_CLOUD_GROQ = "cloud-groq"
#: We could not determine the implementation. Emitted rather than a
#: guess: a wrong label is worse than an honest absence of one.
STT_IMPL_UNKNOWN = "unknown"

# --- stt_accel --------------------------------------------------------

ACCEL_UNKNOWN = "unknown"
ACCEL_CPU = "cpu"
ACCEL_CUDA = "cuda"
ACCEL_VULKAN = "vulkan"

#: The closed vocabulary :func:`normalize_accel` maps onto. Kept in sync
#: with ``Accel::as_str`` in ``src/rust/whisper/accel.rs``.
KNOWN_ACCELS = (ACCEL_CPU, ACCEL_CUDA, ACCEL_VULKAN)

#: Host substring identifying Groq's OpenAI-compatible base URL. Mirrors
#: ``GROQ_HOST_MARKER`` in the Rust module and the provider sniffing in
#: ``vp_external_api._api_key``.
GROQ_HOST_MARKER = "groq.com"


def cloud_stt_impl_for_base_url(base_url: str) -> str:
    """Return the ``stt_impl`` label for a cloud endpoint's ``base_url``.

    Sniffs the host rather than trusting ``stt_backend``, which is
    ``openai`` for Groq too. An empty base URL means the OpenAI default.
    """
    if GROQ_HOST_MARKER in (base_url or "").lower():
        return STT_IMPL_CLOUD_GROQ
    return STT_IMPL_CLOUD_OPENAI


def normalize_accel(raw: Any) -> str:
    """Map a reported compute path onto the closed ``stt_accel`` set.

    Anything unrecognised (including ``None``, ``""``, and CTranslate2's
    ``auto``) becomes :data:`ACCEL_UNKNOWN`. ``auto`` deliberately does
    NOT pass through: it is the request, not the outcome, and letting it
    through would recreate the exact ambiguity these fields remove.
    """
    value = str(raw or "").strip().lower()
    return value if value in KNOWN_ACCELS else ACCEL_UNKNOWN


def _looks_like_faster_whisper(model: Any) -> bool:
    """True when ``model`` is a ``faster_whisper.WhisperModel``.

    Two probes because the class is only importable when faster-whisper is
    installed (it is a lazy import everywhere else in this package, and the
    Rust-helper / cloud paths never install it): the module name, and the
    duck-typed shape (a ``.model`` attribute that is a CTranslate2 model,
    i.e. one carrying a ``.device``).
    """
    if type(model).__module__.split(".")[0] == "faster_whisper":
        return True
    return hasattr(getattr(model, "model", None), "device")


def faster_whisper_accel(model: Any) -> str:
    """Compute path CTranslate2 ACTUALLY placed a faster-whisper model on.

    ``WhisperModel.model`` is the ``ctranslate2.models.Whisper`` instance,
    whose ``.device`` reports the device the weights were really loaded
    onto -- ``cuda`` or ``cpu``, never ``auto``. That is the whole point:
    ``VOICEPI_DEVICE=auto`` resolves to one of them at load time and the
    setting never records which.
    """
    inner = getattr(model, "model", None)
    return normalize_accel(getattr(inner, "device", ""))


def describe_stt_stack(model: Any) -> tuple[str, str]:
    """Return ``(stt_impl, stt_accel)`` for a loaded STT model object.

    Resolution order:

    1. ``model.stt_provenance()`` when the object implements it -- the
       wrappers that KNOW their own provenance (the Rust whisper.cpp
       helpers, the cloud adapter) report it directly, including the
       accelerator the helper sent back on its last response.
    2. A faster-whisper model: read the device off the CTranslate2 model.
    3. Otherwise ``(unknown, unknown)``.

    Never raises: this runs on the per-utterance event path and a
    provenance probe must not be able to lose a user's dictation. A
    misbehaving reporter degrades to ``unknown``.
    """
    reporter = getattr(model, "stt_provenance", None)
    if callable(reporter):
        try:
            impl, accel = reporter()
        except Exception:  # noqa: BLE001 - diagnostics must never break dictation
            return (STT_IMPL_UNKNOWN, ACCEL_UNKNOWN)
        return (str(impl or STT_IMPL_UNKNOWN), normalize_accel(accel))
    if _looks_like_faster_whisper(model):
        return (STT_IMPL_FASTER_WHISPER, faster_whisper_accel(model))
    return (STT_IMPL_UNKNOWN, ACCEL_UNKNOWN)


def startup_line(engine: str, stt_impl: str, accel: str, model: str) -> str:
    """Render the one-line startup summary.

    Byte-identical to ``provenance::startup_line`` in Rust so a log from
    either engine reads the same::

        [runtime] transcribe backend resolved: engine=python-worker impl=faster-whisper accel=cuda model=large-v3-turbo

    An empty ``model`` is omitted entirely rather than emitted blank -- a
    blank value reads as "no model" when it means "not applicable".
    """
    line = (
        "[runtime] transcribe backend resolved: "
        f"engine={engine} impl={stt_impl} accel={accel}"
    )
    model = (model or "").strip()
    if model:
        line += f" model={model}"
    return line
