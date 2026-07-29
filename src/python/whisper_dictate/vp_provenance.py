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

import urllib.parse
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
#: Any OTHER OpenAI-compatible endpoint: a self-hosted server on
#: localhost, Azure OpenAI, a proxy -- ``vp_setup.py`` exposes ``custom``
#: as a first-class provider. Distinct from :data:`STT_IMPL_CLOUD_OPENAI`
#: because OpenAI did not serve that audio. Codex P2 #687 round 3.
STT_IMPL_CLOUD_CUSTOM = "cloud-custom"
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

#: Registrable domains. Mirror ``GROQ_DOMAIN`` / ``OPENAI_DOMAIN`` in
#: ``src/rust/dictate/provenance.rs``.
GROQ_DOMAIN = "groq.com"
OPENAI_DOMAIN = "openai.com"


def _host_of(url: str) -> str:
    """Lowercased hostname of ``url``, trailing DNS root dot stripped.

    Kept local rather than importing ``vp_postprocess._endpoint_provider``
    (which classifies the same domains) because this module is a leaf that
    ``vp_external_api`` imports, and that module is pinned by
    ``test_external_api.py`` to import without pulling in the heavier
    post-processing stack. The rule itself is the repository's standard
    one -- see ``vp_postprocess._endpoint_provider`` and Rust's
    ``cloud_api::transcribe::provider_host``.
    """
    try:
        parsed = urllib.parse.urlparse((url or "").strip())
    except ValueError:
        return ""
    return (parsed.hostname or "").lower().rstrip(".")


def cloud_stt_impl_for_base_url(base_url: str) -> str:
    """Return the ``stt_impl`` label for a cloud endpoint's ``base_url``.

    Classifies on the parsed HOST, not a substring, and not on
    ``stt_backend`` -- which is ``openai`` for EVERY OpenAI-compatible
    endpoint (Groq, Azure, a self-hosted server). Three outcomes,
    fail-open to :data:`STT_IMPL_CLOUD_CUSTOM`:

    * ``groq.com`` / ``*.groq.com`` -> :data:`STT_IMPL_CLOUD_GROQ`
    * ``openai.com`` / ``*.openai.com`` (or an unset URL, which means the
      OpenAI default base URL) -> :data:`STT_IMPL_CLOUD_OPENAI`
    * anything else -> :data:`STT_IMPL_CLOUD_CUSTOM`

    A substring test mislabels both directions:
    ``https://groq.com.attacker.example/v1`` merely *contains*
    ``groq.com``, and ``https://api.groq.com@custom.example/v1`` has host
    ``custom.example`` while containing ``api.groq.com``. Either way the
    record would name a service that never saw the audio -- the same class
    of untruth these fields exist to remove. Codex P2 #687 rounds 2 + 3.
    """
    if not (base_url or "").strip():
        return STT_IMPL_CLOUD_OPENAI
    host = _host_of(base_url)
    if host == GROQ_DOMAIN or host.endswith("." + GROQ_DOMAIN):
        return STT_IMPL_CLOUD_GROQ
    if host == OPENAI_DOMAIN or host.endswith("." + OPENAI_DOMAIN):
        return STT_IMPL_CLOUD_OPENAI
    return STT_IMPL_CLOUD_CUSTOM


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
