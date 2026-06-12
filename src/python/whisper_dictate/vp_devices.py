"""Audio input-device selection shared by the picker AND live capture.

This is the single home for the WASAPI→DirectSound→default-on-Windows host-API
preference. PortAudio exposes every physical mic up to four times on Windows
(once per host API: MME, DirectSound, WASAPI, WDM-KS); MME truncates names to 31
chars and routes through a low-fidelity path, so we collapse to one host API.

Two consumers share the rule via :func:`_select_host_api_index`:

  * the microphone PICKER — :func:`select_input_devices` / :func:`list_input_devices`
    (``vp_events`` re-exports these for the ``--list-audio-devices`` CLI), and
  * live CAPTURE — :func:`resolve_capture_device` (called from
    ``vp_capture._resolve_sounddevice_device``), so the bound device has its
    FULL name and capture runs through WASAPI rather than MME's truncated,
    low-fi entry.

All functions are pure over injected ``sd.query_devices()`` /
``sd.query_hostapis()`` sequences (no real audio stack), so they unit-test with
stubbed tables. On a single-host-API platform (Linux/macOS ALSA/CoreAudio)
resolution is unchanged.
"""
from __future__ import annotations

import os


def _default_input_index(sd) -> int | None:
    """The index of sounddevice's default input device, or ``None``.

    Mirrors the default-device resolution used elsewhere: ``sd.default.device``
    is either ``(input, output)`` or a single int; a value of ``-1`` means "no
    explicit default".
    """
    default_device = getattr(getattr(sd, "default", None), "device", None)
    if isinstance(default_device, (list, tuple)) and default_device:
        candidate = default_device[0]
    elif isinstance(default_device, int):
        candidate = default_device
    else:
        return None
    if not isinstance(candidate, int) or candidate < 0:
        return None
    return candidate


def _select_host_api_index(hostapis, *, is_windows: bool) -> int | None:
    """Pick the single host API to enumerate input devices from.

    On Windows, PortAudio exposes every physical mic up to four times — once per
    host API (MME, DirectSound, WASAPI, WDM-KS) — and MME truncates names to 31
    chars while WDM-KS/Sound-Mapper add pseudo-device noise. We collapse that by
    enumerating exactly one host API:

      * Windows: prefer the WASAPI host API (modern, full names, each device
        once); fall back to DirectSound; else the PortAudio default host API.
      * Non-Windows (Linux/macOS): use the PortAudio default host API
        (ALSA / CoreAudio) — never hardcode WASAPI.

    ``hostapis`` is ``sd.query_hostapis()`` (a sequence of dicts). Returns the
    chosen host API's *index* (its position in that sequence), or ``None`` only
    when the list is EMPTY (older sounddevice without a host-API table) so the
    caller falls back to enumerating every device. A non-empty list always
    yields an index — a preferred API if matched, else the first with a default
    input, else ``0``.
    """
    apis = list(hostapis or [])
    if not apis:
        return None

    def _name(api) -> str:
        return str((api or {}).get("name") or "") if isinstance(api, dict) else ""

    if is_windows:
        for needle in ("wasapi", "directsound"):
            for index, api in enumerate(apis):
                if needle in _name(api).casefold():
                    return index

    # Default host API: PortAudio marks it via the per-API ``default_input_device``
    # being a real device, but the simplest portable signal is the first API whose
    # ``default_input_device`` is set; otherwise fall back to host API 0.
    for index, api in enumerate(apis):
        if not isinstance(api, dict):
            continue
        default_input = api.get("default_input_device")
        if isinstance(default_input, int) and default_input >= 0:
            return index
    return 0


def _is_wasapi_device(devices, hostapis, device) -> bool:
    """True if ``device`` (a query_devices index) belongs to a WASAPI host API.

    Used to decide whether a WASAPI ``auto_convert`` stream candidate is worth
    trying (so the device can resample 16k internally on machines that reject
    16k shared-mode). Defensive against stub/legacy sequences and non-int
    devices — returns ``False`` whenever WASAPI membership can't be established.
    """
    if not isinstance(device, int):
        return False
    if not isinstance(devices, (list, tuple)) or not (0 <= device < len(devices)):
        return False
    info = devices[device]
    if not isinstance(info, dict):
        return False
    hostapi = info.get("hostapi")
    apis = list(hostapis or [])
    if not isinstance(hostapi, int) or not (0 <= hostapi < len(apis)):
        return False
    api = apis[hostapi]
    if not isinstance(api, dict):
        return False
    return "wasapi" in str(api.get("name") or "").casefold()


def _name_matches(needle: str, name: str) -> bool:
    """True if a saved device value and a candidate name refer to each other.

    Matches case-insensitively when either string is a substring of the other.
    The bidirectional test tolerates an MME-truncated saved value (31 chars)
    still resolving to the full WASAPI/DirectSound name (e.g. a saved
    ``"Headset Microphone (Jabra Evolv"`` matching the full
    ``"Headset Microphone (Jabra Evolve 65 TE)"``).
    """
    needle = needle.casefold()
    name = name.casefold()
    if not needle or not name:
        return False
    return needle in name or name in needle


def _hostapi_name_at(hostapis, hostapi_index) -> str:
    """Best-effort host-API name for a ``hostapi`` index (``""`` when unknown).

    Pure over an injected ``sd.query_hostapis()`` sequence so the sibling-endpoint
    resolver can label each endpoint (``"Windows WASAPI"`` / ``"Windows
    DirectSound"`` / ``"MME"``) without touching the audio stack.
    """
    apis = list(hostapis or [])
    if not isinstance(hostapi_index, int) or not (0 <= hostapi_index < len(apis)):
        return ""
    api = apis[hostapi_index]
    if not isinstance(api, dict):
        return ""
    return str(api.get("name") or "").strip()


def sibling_endpoints_for_device(sd, device) -> list[tuple[int, str]]:
    """The SAME physical mic's input endpoints across host APIs, open-first order.

    PortAudio exposes one physical mic up to four times on Windows — once per
    host API (MME, DirectSound, WASAPI, WDM-KS). When the resolved (WASAPI)
    endpoint of an explicitly-chosen mic refuses to open across the whole
    format/rate/dtype matrix, capture should retry the SAME physical device via
    its DirectSound / MME siblings BEFORE silently swapping to a *different* mic
    (which records the wrong input). This finds those siblings.

    Given a resolved ``device`` (a ``query_devices`` index), returns
    ``[(index, hostapi_name), …]`` for every input endpoint whose name refers to
    the same physical device, in OPEN-PREFERENCE order:

      1. the resolved endpoint itself (so the caller can keep a single loop),
      2. its DirectSound sibling(s) — cheapest non-WASAPI path, accepts 16k int16
         directly (no WASAPI ``auto_convert``, no resample),
      3. its MME sibling(s) — last resort (low-fidelity, name truncated to 31
         chars; matched on a bidirectional-substring basis via
         :func:`_name_matches`, so the MME 31-char name still maps to the same
         physical device as the full WASAPI/DirectSound name).

    Other host APIs (e.g. WDM-KS) and the resolved endpoint's own host API are
    skipped as siblings (we never re-add the resolved endpoint or pull in
    pseudo-device noise). Pure over injected ``sd.query_devices()`` /
    ``sd.query_hostapis()`` so it unit-tests with the same stubbed tables the
    picker/resolver use. Any query failure or a non-int ``device`` yields just
    ``[(device, name)]`` (or ``[]``), so capture degrades to today's behaviour.
    """
    if not isinstance(device, int):
        return []
    try:
        devices = list(sd.query_devices())
    except Exception:
        return []
    if not (0 <= device < len(devices)):
        return []
    info = devices[device]
    if not isinstance(info, dict):
        return []
    try:
        hostapis = list(sd.query_hostapis())
    except Exception:
        hostapis = []

    resolved_name = str(info.get("name") or "").strip()
    resolved_api = info.get("hostapi")
    result: list[tuple[int, str]] = [
        (device, _hostapi_name_at(hostapis, resolved_api))]
    if not resolved_name:
        return result

    def _api_rank(api_name: str) -> int | None:
        folded = api_name.casefold()
        if "directsound" in folded:
            return 0  # try DirectSound before MME
        if "mme" in folded:
            return 1
        return None  # WDM-KS / unknown: never a sibling fallback

    siblings: list[tuple[int, int, str]] = []  # (rank, index, hostapi_name)
    for index, entry in enumerate(devices):
        if index == device or not isinstance(entry, dict):
            continue
        if entry.get("hostapi") == resolved_api:
            continue  # same host API as the resolved endpoint — not a sibling
        try:
            channels = int(entry.get("max_input_channels") or 0)
        except (TypeError, ValueError):
            channels = 0
        if channels <= 0:
            continue
        name = str(entry.get("name") or "").strip()
        if not name or not _name_matches(resolved_name, name):
            continue
        api_name = _hostapi_name_at(hostapis, entry.get("hostapi"))
        rank = _api_rank(api_name)
        if rank is None:
            continue
        siblings.append((rank, index, api_name))

    # DirectSound (rank 0) before MME (rank 1); stable index order within a rank.
    siblings.sort(key=lambda item: (item[0], item[1]))
    result.extend((index, api_name) for _rank, index, api_name in siblings)
    return result


def resolve_capture_device(
    devices,
    hostapis,
    value: str,
    *,
    is_windows: bool,
    default_index: int | None,
) -> tuple[int | None, str | None]:
    """Resolve a saved ``VOICEPI_AUDIO_DEVICE`` value to ``(index, full_name)``.

    Pure host-API-aware capture resolution, reusing :func:`_select_host_api_index`
    so capture binds the SAME WASAPI→DirectSound→default host API the picker
    enumerates. This is why the worker's ``audio_device`` field carries the full
    device name rather than MME's 31-char truncation, and why capture runs over
    WASAPI instead of MME's low-fidelity path on Windows.

    ``value`` semantics:
      * empty/unset       → pick the preferred host API's DEFAULT input device
        (so the global MME default is never used); ``(index, full_name)`` or
        ``(None, None)`` when no default can be determined.
      * an integer string → that explicit device index, used verbatim
        (``(index, None)`` — the caller already trusts the index).
      * a name substring  → the matching input device IN THE PREFERRED HOST API
        (full name); if no preferred-API device matches, the first match across
        any host API; ``(None, None)`` when nothing matches (caller warns + uses
        default).

    Name matching prefers a case-insensitive EXACT match first (so a saved name
    that is a clean prefix of a longer sibling — "Microphone" vs "Microphone
    Array" — binds the exact device, not the sibling). Failing an exact hit it
    falls back to a bidirectional-substring match (see :func:`_name_matches`) so
    an old MME-truncated saved name still resolves to the full WASAPI device,
    preferring the longest (fullest) matching name within the chosen host API.

    Returns ``(index, full_name)``. ``full_name`` may be ``None`` when only an
    index is known (explicit numeric value); the caller then resolves the label
    separately.
    """
    value = (value or "").strip()
    if value and value.lstrip("+-").isdigit():
        return int(value), None

    # Defensive: only a real sequence is index-addressable. A stub/legacy
    # ``query_devices()`` that returns a single dict (or anything non-sequence)
    # yields no resolution → caller uses sounddevice's own default.
    if not isinstance(devices, (list, tuple)):
        devices = []

    chosen = _select_host_api_index(hostapis, is_windows=is_windows)

    def _inputs_in(hostapi_filter):
        out = []
        for index, info in enumerate(devices):
            if not isinstance(info, dict):
                continue
            if hostapi_filter is not None and info.get("hostapi") != hostapi_filter:
                continue
            try:
                channels = int(info.get("max_input_channels") or 0)
            except (TypeError, ValueError):
                channels = 0
            if channels <= 0:
                continue
            name = str(info.get("name") or "").strip()
            out.append((index, name))
        return out

    if not value:
        # Default fallback: the preferred host API's own default input device,
        # so we bind the full-name WASAPI/DirectSound default — never the MME
        # global default. PortAudio exposes this as the host API's
        # ``default_input_device`` (a global query_devices index).
        apis = list(hostapis or [])
        if chosen is not None and 0 <= chosen < len(apis):
            api = apis[chosen]
            if isinstance(api, dict):
                default_input = api.get("default_input_device")
                if isinstance(default_input, int) and 0 <= default_input < len(devices):
                    info = devices[default_input]
                    if isinstance(info, dict):
                        name = str(info.get("name") or "").strip()
                        return default_input, (name or None)
        # No preferred-API default → fall back to sounddevice's global default.
        if isinstance(default_index, int) and 0 <= default_index < len(devices):
            info = devices[default_index]
            if isinstance(info, dict):
                name = str(info.get("name") or "").strip()
                return default_index, (name or None)
        return None, None

    # Named value: a case-insensitive EXACT name match wins immediately, so a
    # saved value that is a clean prefix of a longer sibling (e.g. "Microphone"
    # vs "Microphone Array" in the same host API) can never be hijacked by the
    # longest-substring rule below. Only when no exact match exists do we fall
    # back to the longest (fullest) bidirectional-substring match — that still
    # resolves an MME-truncated saved value to its single full WASAPI name.
    # Fall back to any host-API match so behaviour never regresses.
    folded = value.casefold()

    def _best_match(candidates):
        best = None
        for index, name in candidates:
            if name.casefold() == folded:
                return (index, name)
            if _name_matches(value, name):
                if best is None or len(name) > len(best[1]):
                    best = (index, name)
        return best

    if chosen is not None:
        hit = _best_match(_inputs_in(chosen))
        if hit is not None:
            return hit[0], (hit[1] or None)
    hit = _best_match(_inputs_in(None))
    if hit is not None:
        return hit[0], (hit[1] or None)
    return None, None


def select_input_devices(devices, hostapis, *, is_windows: bool, default_index: int | None) -> list[dict]:
    """Pure host-API selection + filtering for the microphone picker.

    Given ``sd.query_devices()`` (``devices``) and ``sd.query_hostapis()``
    (``hostapis``), choose ONE host API (see :func:`_select_host_api_index`) and
    return each of its real input devices once:

      * ``hostapi`` == the chosen host-API index,
      * ``max_input_channels > 0``,
      * non-empty name (blank names collide with the UI's "(System default)").

    The returned ``index`` is preserved only for the JSON contract / manual
    numeric entry: the Rust picker discards it and persists the device NAME, and
    capture (:func:`resolve_capture_device`) re-resolves that name against the
    SAME preferred host API — so the picker and capture agree on the physical
    device and its full name. ``default`` is set on the entry whose index ==
    ``default_index`` (sounddevice's default input). Kept pure so it is
    unit-testable with stubbed sequences.
    """
    chosen = _select_host_api_index(hostapis, is_windows=is_windows)
    result: list[dict] = []
    for index, info in enumerate(devices):
        if not isinstance(info, dict):
            continue
        if chosen is not None and info.get("hostapi") != chosen:
            continue
        try:
            channels = int(info.get("max_input_channels") or 0)
        except (TypeError, ValueError):
            channels = 0
        if channels <= 0:
            continue
        name = str(info.get("name") or "").strip()
        if not name:
            # An empty name would collide with the UI's "" = "(System default)"
            # combo value and make the selection ambiguous — skip the entry.
            continue
        result.append({
            "index": index,
            "name": name,
            "max_input_channels": channels,
            "default": index == default_index,
        })
    return result


def list_input_devices(sd) -> list[dict]:
    """Return input devices for the picker, enumerated from a single host API.

    Each entry is ``{"index", "name", "max_input_channels", "default"}``. The
    real work (host-API selection + filtering) lives in
    :func:`select_input_devices`; this just reads the live sounddevice tables and
    delegates. Pure over an injected ``sd`` so it is unit-testable with a stubbed
    sounddevice.
    """
    default_index = _default_input_index(sd)
    devices = sd.query_devices()
    try:
        hostapis = sd.query_hostapis()
    except Exception:
        hostapis = []
    return select_input_devices(
        devices,
        hostapis,
        is_windows=(os.name == "nt"),
        default_index=default_index,
    )
