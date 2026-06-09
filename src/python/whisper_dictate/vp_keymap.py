"""Keyboard-layout (XKB) detection for Wayland text injection.

Extracted from runtime.py. Maps a dictation language (or an explicit override)
to a supported XKB layout so ydotool / the Rust keymap inject characters with the
right layout. Pure stdlib; both the live Dictate loop and main() use it.
"""
from __future__ import annotations

import os
import re

_LANG_TO_XKB = {
    "da": "dk", "de": "de", "fr": "fr", "fi": "fi", "sv": "se",
    "nb": "no", "nn": "no", "nl": "nl", "pl": "pl", "pt": "pt",
    "es": "es", "it": "it", "uk": "ua",
}
_SUPPORTED_XKB_LAYOUTS = {
    "br", "de", "dk", "es", "fi", "fr", "it", "no", "pl", "pt", "se", "ua", "us",
}


def _normalize_xkb_layout(layout: str | None) -> str | None:
    raw = (layout or "").strip()
    if not raw:
        return None
    mapped = _LANG_TO_XKB.get(raw, raw)
    if mapped in _SUPPORTED_XKB_LAYOUTS:
        return mapped
    return None


def _detect_xkb_layout(lang: str | None = None) -> str | None:
    for var in ("VOICEPI_XKB_LAYOUT", "XKB_DEFAULT_LAYOUT"):
        layout = _normalize_xkb_layout(os.environ.get(var, ""))
        if layout:
            return layout
    try:
        with open("/etc/default/keyboard", encoding="utf-8") as f:
            for line in f:
                match = re.match(r'XKBLAYOUT="?([^"\s]+)"?', line)
                if match:
                    layout = _normalize_xkb_layout(match.group(1))
                    if layout != "us":
                        return layout
    except FileNotFoundError:
        pass
    return _normalize_xkb_layout(lang)
