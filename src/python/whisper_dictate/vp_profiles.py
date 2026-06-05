"""Target profile matching for per-app/per-window dictation settings."""
from __future__ import annotations

from typing import Any


def _values(raw: Any) -> list[str]:
    if raw is None:
        return []
    if isinstance(raw, str):
        return [raw]
    if isinstance(raw, (list, tuple)):
        return [str(item) for item in raw if str(item).strip()]
    return [str(raw)]


def _contains_any(haystack: str | None, needles: Any) -> bool:
    vals = [v.casefold() for v in _values(needles) if v.strip()]
    if not vals:
        return True
    text = (haystack or "").casefold()
    return any(value in text for value in vals)


def match_profile(
    profiles: list[dict[str, Any]] | Any,
    *,
    title: str | None,
    process: str | None,
) -> tuple[str | None, dict[str, str]]:
    if not isinstance(profiles, list):
        return None, {}
    for profile in profiles:
        if not isinstance(profile, dict):
            continue
        match = profile.get("match", {})
        if not isinstance(match, dict):
            continue
        if not _contains_any(title, match.get("title")):
            continue
        if not _contains_any(process, match.get("process")):
            continue
        settings = profile.get("settings", {})
        if not isinstance(settings, dict):
            settings = {}
        name = str(profile.get("name") or "unnamed")
        return name, {str(k): str(v) for k, v in settings.items() if v not in (None, "")}
    return None, {}


def apply_profile_settings(
    base: dict[str, str],
    profiles: list[dict[str, Any]] | Any,
    *,
    title: str | None,
    process: str | None,
) -> tuple[dict[str, str], str | None]:
    name, settings = match_profile(profiles, title=title, process=process)
    if not settings:
        return dict(base), name
    out = dict(base)
    out.update(settings)
    return out, name
