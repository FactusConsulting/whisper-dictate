"""Wayland XKB-layout keycode maps + ydotool op segmentation.

Pure, dependency-free (stdlib only) — extracted from voice_pi.py so it
can be unit-tested in isolation. Behaviour is unchanged: this is a
verbatim move; the existing test suite is the behaviour contract.
"""
from __future__ import annotations

import os
import re

_LANG_TO_XKB = {
    "da": "dk", "de": "de", "fr": "fr", "fi": "fi", "sv": "se",
    "nb": "no", "nn": "no", "nl": "nl", "pl": "pl", "pt": "pt",
    "es": "es", "it": "it", "uk": "ua",
}

# Wayland text injection: ydotool type bruger US-keyboard internt.
# Tegn placeret anderledes i ikke-US layouts skal sendes som raw evdev-keycodes
# via ydotool key så compositor anvender det aktive XKB-layout korrekt.
#
# Relevante scancodes (Linux input-event-codes.h):
#   KEY_2=3  KEY_7=8  KEY_MINUS=12  KEY_LEFTBRACE=26
#   KEY_SEMICOLON=39  KEY_APOSTROPHE=40  KEY_LEFTSHIFT=42
#   KEY_COMMA=51  KEY_DOT=52  KEY_SLASH=53

# Tegnsætning der er identisk placeret i alle nordiske + tyske layouts,
# men anderledes end US (f.eks. ? er shift+KEY_MINUS, ikke shift+KEY_SLASH).
_NORDIC_DE_PUNCT: dict[str, list[str]] = {
    '?': ['42:1', '12:1', '12:0', '42:0'],  # shift+KEY_MINUS (US: shift+KEY_SLASH)
    '-': ['53:1', '53:0'],                   # KEY_SLASH       (US: KEY_MINUS)
    '_': ['42:1', '53:1', '53:0', '42:0'],  # shift+KEY_SLASH
    ':': ['42:1', '52:1', '52:0', '42:0'],  # shift+KEY_DOT   (US: shift+KEY_SEMICOLON)
    ';': ['42:1', '51:1', '51:0', '42:0'],  # shift+KEY_COMMA (US: KEY_SEMICOLON)
    '/': ['42:1', '8:1', '8:0', '42:0'],    # shift+KEY_7     (US: KEY_SLASH)
    '"': ['42:1', '3:1', '3:0', '42:0'],    # shift+KEY_2     (US: shift+KEY_APOSTROPHE)
}

# Hjælpefunktioner til at bygge dead-key-sekvenser.
# dead(dk) + plain(lc): fx dead_acute(40) + 'a'(30) → á
def _dead(dk: int, lc: int) -> list[str]:
    return [f'{dk}:1', f'{dk}:0', f'{lc}:1', f'{lc}:0']

def _dead_up(dk: int, lc: int) -> list[str]:
    return [f'{dk}:1', f'{dk}:0', '42:1', f'{lc}:1', f'{lc}:0', '42:0']

def _shift_dead(dk: int, lc: int) -> list[str]:
    return ['42:1', f'{dk}:1', f'{dk}:0', '42:0', f'{lc}:1', f'{lc}:0']

def _shift_dead_up(dk: int, lc: int) -> list[str]:
    return ['42:1', f'{dk}:1', f'{dk}:0', '42:0', '42:1', f'{lc}:1', f'{lc}:0', '42:0']

def _altgr(lc: int) -> list[str]:
    return ['100:1', f'{lc}:1', f'{lc}:0', '100:0']

def _altgr_up(lc: int) -> list[str]:
    return ['100:1', '42:1', f'{lc}:1', f'{lc}:0', '42:0', '100:0']

# Per-layout keycode-kort: XKB-layoutnavn → tegn → ydotool key-sekvens.
# Keycodes 26/39/40 + shift(42) er de nordiske/tyske specialtegntaster —
# samme fysiske placering, forskelligt tegn afhængig af layout.
_LAYOUT_KEYCODES: dict[str, dict[str, list[str]]] = {
    'dk': {  # Dansk: å æ ø
        'å': ['26:1', '26:0'], 'Å': ['42:1', '26:1', '26:0', '42:0'],
        'æ': ['39:1', '39:0'], 'Æ': ['42:1', '39:1', '39:0', '42:0'],
        'ø': ['40:1', '40:0'], 'Ø': ['42:1', '40:1', '40:0', '42:0'],
        **_NORDIC_DE_PUNCT,
    },
    'se': {  # Svensk: å ä ö (ä og ö på samme keycodes som DK's ø og æ)
        'å': ['26:1', '26:0'], 'Å': ['42:1', '26:1', '26:0', '42:0'],
        'ä': ['40:1', '40:0'], 'Ä': ['42:1', '40:1', '40:0', '42:0'],
        'ö': ['39:1', '39:0'], 'Ö': ['42:1', '39:1', '39:0', '42:0'],
        **_NORDIC_DE_PUNCT,
    },
    'de': {  # Tysk: ä ö ü (samme keycodes som nordiske specialtegn)
        'ä': ['40:1', '40:0'], 'Ä': ['42:1', '40:1', '40:0', '42:0'],
        'ö': ['39:1', '39:0'], 'Ö': ['42:1', '39:1', '39:0', '42:0'],
        'ü': ['26:1', '26:0'], 'Ü': ['42:1', '26:1', '26:0', '42:0'],
        **_NORDIC_DE_PUNCT,
    },
    'fi': {  # Finsk: ä ö (ingen å i normal finsk tekst)
        'ä': ['40:1', '40:0'], 'Ä': ['42:1', '40:1', '40:0', '42:0'],
        'ö': ['39:1', '39:0'], 'Ö': ['42:1', '39:1', '39:0', '42:0'],
        **_NORDIC_DE_PUNCT,
    },
    'es': {  # Spansk: ñ direkte; betonede vokaler via dead_acute (AC11=40)
        'ñ': ['39:1', '39:0'], 'Ñ': ['42:1', '39:1', '39:0', '42:0'],
        'á': _dead(40, 30), 'Á': _dead_up(40, 30),
        'é': _dead(40, 18), 'É': _dead_up(40, 18),
        'í': _dead(40, 23), 'Í': _dead_up(40, 23),
        'ó': _dead(40, 24), 'Ó': _dead_up(40, 24),
        'ú': _dead(40, 22), 'Ú': _dead_up(40, 22),
        'ü': _shift_dead(40, 22), 'Ü': _shift_dead_up(40, 22),
    },
    'pt': {  # Portugisisk (EU): ç direkte; vokaler via dead keys
        # AC10(39)=ç  AD12(27)=dead_acute  BKSL(43)=dead_tilde  shift+BKSL=dead_circumflex
        'ç': ['39:1', '39:0'], 'Ç': ['42:1', '39:1', '39:0', '42:0'],
        'á': _dead(27, 30), 'Á': _dead_up(27, 30),
        'é': _dead(27, 18), 'É': _dead_up(27, 18),
        'í': _dead(27, 23), 'Í': _dead_up(27, 23),
        'ó': _dead(27, 24), 'Ó': _dead_up(27, 24),
        'ú': _dead(27, 22), 'Ú': _dead_up(27, 22),
        'à': _shift_dead(27, 30), 'À': _shift_dead_up(27, 30),
        'ã': _dead(43, 30), 'Ã': _dead_up(43, 30),
        'õ': _dead(43, 24), 'Õ': _dead_up(43, 24),
        'â': _shift_dead(43, 30), 'Â': _shift_dead_up(43, 30),
        'ê': _shift_dead(43, 18), 'Ê': _shift_dead_up(43, 18),
        'ô': _shift_dead(43, 24), 'Ô': _shift_dead_up(43, 24),
    },
    'br': {  # Portugisisk (BR): ç direkte; AC11(40)=dead_tilde/dead_circumflex
        'ç': ['39:1', '39:0'], 'Ç': ['42:1', '39:1', '39:0', '42:0'],
        'ã': _dead(40, 30), 'Ã': _dead_up(40, 30),
        'õ': _dead(40, 24), 'Õ': _dead_up(40, 24),
        'â': _shift_dead(40, 30), 'Â': _shift_dead_up(40, 30),
        'ê': _shift_dead(40, 18), 'Ê': _shift_dead_up(40, 18),
        'ô': _shift_dead(40, 24), 'Ô': _shift_dead_up(40, 24),
        # dead_acute via AltGr+AC10(39)
        'á': _dead(39, 30), 'Á': _dead_up(39, 30),  # ydotool type via altgr ikke understøttet
        'é': _dead(39, 18), 'É': _dead_up(39, 18),
        'í': _dead(39, 23), 'Í': _dead_up(39, 23),
        'ó': _dead(39, 24), 'Ó': _dead_up(39, 24),
        'ú': _dead(39, 22), 'Ú': _dead_up(39, 22),
    },
    'pl': {  # Polsk: alle via AltGr+bogstav (KEY_RIGHTALT=100)
        'ą': _altgr(30), 'Ą': _altgr_up(30),  # AltGr+a
        'ę': _altgr(18), 'Ę': _altgr_up(18),  # AltGr+e
        'ó': _altgr(24), 'Ó': _altgr_up(24),  # AltGr+o
        'ś': _altgr(31), 'Ś': _altgr_up(31),  # AltGr+s
        'ź': _altgr(45), 'Ź': _altgr_up(45),  # AltGr+x
        'ż': _altgr(44), 'Ż': _altgr_up(44),  # AltGr+z
        'ć': _altgr(46), 'Ć': _altgr_up(46),  # AltGr+c
        'ń': _altgr(49), 'Ń': _altgr_up(49),  # AltGr+n
        'ł': _altgr(38), 'Ł': _altgr_up(38),  # AltGr+l
    },
    'ua': {  # Ukrainsk: hele det kyrilliske alfabet som direkte keycodes
        # AD-række: й ц у к е н г ш щ з х ї
        'й': ['16:1', '16:0'], 'Й': ['42:1', '16:1', '16:0', '42:0'],
        'ц': ['17:1', '17:0'], 'Ц': ['42:1', '17:1', '17:0', '42:0'],
        'у': ['18:1', '18:0'], 'У': ['42:1', '18:1', '18:0', '42:0'],
        'к': ['19:1', '19:0'], 'К': ['42:1', '19:1', '19:0', '42:0'],
        'е': ['20:1', '20:0'], 'Е': ['42:1', '20:1', '20:0', '42:0'],
        'н': ['21:1', '21:0'], 'Н': ['42:1', '21:1', '21:0', '42:0'],
        'г': ['22:1', '22:0'], 'Г': ['42:1', '22:1', '22:0', '42:0'],
        'ш': ['23:1', '23:0'], 'Ш': ['42:1', '23:1', '23:0', '42:0'],
        'щ': ['24:1', '24:0'], 'Щ': ['42:1', '24:1', '24:0', '42:0'],
        'з': ['25:1', '25:0'], 'З': ['42:1', '25:1', '25:0', '42:0'],
        'х': ['26:1', '26:0'], 'Х': ['42:1', '26:1', '26:0', '42:0'],
        'ї': ['27:1', '27:0'], 'Ї': ['42:1', '27:1', '27:0', '42:0'],
        # AC-række: ф і в а п р о л д ж є
        'ф': ['30:1', '30:0'], 'Ф': ['42:1', '30:1', '30:0', '42:0'],
        'і': ['31:1', '31:0'], 'І': ['42:1', '31:1', '31:0', '42:0'],
        'в': ['32:1', '32:0'], 'В': ['42:1', '32:1', '32:0', '42:0'],
        'а': ['33:1', '33:0'], 'А': ['42:1', '33:1', '33:0', '42:0'],
        'п': ['34:1', '34:0'], 'П': ['42:1', '34:1', '34:0', '42:0'],
        'р': ['35:1', '35:0'], 'Р': ['42:1', '35:1', '35:0', '42:0'],
        'о': ['36:1', '36:0'], 'О': ['42:1', '36:1', '36:0', '42:0'],
        'л': ['37:1', '37:0'], 'Л': ['42:1', '37:1', '37:0', '42:0'],
        'д': ['38:1', '38:0'], 'Д': ['42:1', '38:1', '38:0', '42:0'],
        'ж': ['39:1', '39:0'], 'Ж': ['42:1', '39:1', '39:0', '42:0'],
        'є': ['40:1', '40:0'], 'Є': ['42:1', '40:1', '40:0', '42:0'],
        'ґ': ['43:1', '43:0'], 'Ґ': ['42:1', '43:1', '43:0', '42:0'],
        # AB-række: я ч с м и т ь б ю
        'я': ['44:1', '44:0'], 'Я': ['42:1', '44:1', '44:0', '42:0'],
        'ч': ['45:1', '45:0'], 'Ч': ['42:1', '45:1', '45:0', '42:0'],
        'с': ['46:1', '46:0'], 'С': ['42:1', '46:1', '46:0', '42:0'],
        'м': ['47:1', '47:0'], 'М': ['42:1', '47:1', '47:0', '42:0'],
        'и': ['48:1', '48:0'], 'И': ['42:1', '48:1', '48:0', '42:0'],
        'т': ['49:1', '49:0'], 'Т': ['42:1', '49:1', '49:0', '42:0'],
        'ь': ['50:1', '50:0'], 'Ь': ['42:1', '50:1', '50:0', '42:0'],
        'б': ['51:1', '51:0'], 'Б': ['42:1', '51:1', '51:0', '42:0'],
        'ю': ['52:1', '52:0'], 'Ю': ['42:1', '52:1', '52:0', '42:0'],
    },
}
# Norsk layout er identisk med dansk for æ, ø, å
_LAYOUT_KEYCODES['no'] = _LAYOUT_KEYCODES['dk']

_SUPPORTED_XKB_LAYOUTS = set(_LAYOUT_KEYCODES) | {"us"}


def _normalize_xkb_layout(layout: str | None) -> str | None:
    raw = (layout or "").strip()
    if not raw:
        return None
    mapped = _LANG_TO_XKB.get(raw, raw)
    if mapped in _SUPPORTED_XKB_LAYOUTS:
        return mapped
    return None


def _build_ydotool_ops(
    text: str,
    keycode_map: dict[str, list[str]],
) -> list[tuple[str, ...]]:
    """Split text into ydotool (subcommand, *args) tuples.

    Characters in keycode_map become ('key', code, ...) events so the
    compositor applies the active XKB layout.  Remaining characters are
    batched into ('type', '--', chunk) calls to minimise process spawns.
    """
    ops: list[tuple[str, ...]] = []
    buf: list[str] = []
    for ch in text:
        if ch in keycode_map:
            if buf:
                ops.append(('type', '--', ''.join(buf)))
                buf = []
            ops.append(('key', *keycode_map[ch]))
        else:
            buf.append(ch)
    if buf:
        ops.append(('type', '--', ''.join(buf)))
    return ops


def _detect_xkb_layout(lang: str | None = None) -> str | None:
    # Priority: VOICEPI_XKB_LAYOUT > XKB_DEFAULT_LAYOUT > /etc/default/keyboard > lang hint
    for var in ("VOICEPI_XKB_LAYOUT", "XKB_DEFAULT_LAYOUT"):
        layout = _normalize_xkb_layout(os.environ.get(var, ""))
        if layout:
            return layout
    try:
        with open("/etc/default/keyboard") as f:
            for line in f:
                m = re.match(r'XKBLAYOUT="?([^"\s]+)"?', line)
                if m:
                    layout = _normalize_xkb_layout(m.group(1))
                    if layout != "us":  # "us" is often wrong on non-US systems
                        return layout
    except FileNotFoundError:
        pass
    # Fall back: derive layout from spoken-language hint (da→dk, de→de, sv→se…)
    layout = _normalize_xkb_layout(lang)
    if layout:
        return layout
    return None
