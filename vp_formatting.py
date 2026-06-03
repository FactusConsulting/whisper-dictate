"""Deterministic spoken formatting commands."""
from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Iterable

from vp_config import get_value
from vp_metrics import truthy


@dataclass(frozen=True)
class FormatCommandResult:
    text: str
    enabled: bool
    changed: bool = False
    command_set: str = "off"
    applied: list[dict[str, str]] = field(default_factory=list)


_EN_COMMANDS: tuple[tuple[str, str], ...] = (
    ("new paragraph", "\n\n"),
    ("new line", "\n"),
    ("newline", "\n"),
    ("bullet list", "\n- "),
    ("bullet point", "\n- "),
    ("comma", ","),
    ("period", "."),
    ("full stop", "."),
    ("question mark", "?"),
    ("exclamation mark", "!"),
    ("colon", ":"),
    ("semicolon", ";"),
    ("dash", "-"),
    ("hyphen", "-"),
)

_DA_COMMANDS: tuple[tuple[str, str], ...] = (
    ("nyt afsnit", "\n\n"),
    ("ny linje", "\n"),
    ("linjeskift", "\n"),
    ("punktliste", "\n- "),
    ("punktopstilling", "\n- "),
    ("komma", ","),
    ("punktum", "."),
    ("spørgsmålstegn", "?"),
    ("sporgsmålstegn", "?"),
    ("udråbstegn", "!"),
    ("udraabstegn", "!"),
    ("kolon", ":"),
    ("semikolon", ";"),
    ("bindestreg", "-"),
)


def _command_set() -> str:
    raw = (get_value("VOICEPI_FORMAT_COMMANDS", "off") or "off").strip().lower()
    if truthy(raw) and raw not in ("en", "da", "both", "all"):
        return "both"
    if raw == "all":
        return "both"
    if raw in ("en", "da", "both"):
        return raw
    return "off"


def _commands_for(command_set: str) -> Iterable[tuple[str, str]]:
    if command_set in ("en", "both"):
        yield from _EN_COMMANDS
    if command_set in ("da", "both"):
        yield from _DA_COMMANDS


def _pattern(phrase: str) -> re.Pattern[str]:
    words = [re.escape(p) for p in phrase.split()]
    body = r"\s+".join(words)
    return re.compile(rf"(?<!\w){body}(?!\w)[ \t,.!?;:]*", re.IGNORECASE)


def _strip_horizontal_space_around_newlines(text: str) -> str:
    return "\n".join(part.strip(" \t") for part in text.split("\n"))


def _normalize_punctuation_spacing(text: str) -> str:
    punctuation = set(",.;:!?")
    out: list[str] = []
    for idx, char in enumerate(text):
        if char in punctuation:
            while out and out[-1] in (" ", "\t"):
                out.pop()
            out.append(char)
            next_char = text[idx + 1] if idx + 1 < len(text) else ""
            if next_char and not next_char.isspace():
                out.append(" ")
            continue
        out.append(char)
    return "".join(out)


def _normalize_dash_spacing(text: str) -> str:
    out: list[str] = []
    idx = 0
    while idx < len(text):
        char = text[idx]
        if char != "-":
            out.append(char)
            idx += 1
            continue

        while out and out[-1] in (" ", "\t"):
            out.pop()
        out.append(" - ")
        idx += 1
        while idx < len(text) and text[idx] in (" ", "\t"):
            idx += 1
    return "".join(out).replace("\n - ", "\n- ")


def _collapse_extra_newlines(text: str) -> str:
    out: list[str] = []
    newline_count = 0
    for char in text:
        if char == "\n":
            newline_count += 1
            if newline_count <= 2:
                out.append(char)
            continue
        newline_count = 0
        out.append(char)
    return "".join(out)


def _tidy(text: str) -> str:
    text = _strip_horizontal_space_around_newlines(text)
    text = _normalize_punctuation_spacing(text)
    text = _normalize_dash_spacing(text)
    return _collapse_extra_newlines(text).strip()


def apply_format_commands(text: str, command_set: str | None = None) -> FormatCommandResult:
    selected = (command_set or _command_set()).strip().lower()
    if selected in ("0", "false", "no", "off", ""):
        return FormatCommandResult(text=text, enabled=False, command_set="off")
    if selected == "all":
        selected = "both"
    if selected not in ("en", "da", "both"):
        selected = "both"

    out = text
    applied: list[dict[str, str]] = []
    for phrase, replacement in sorted(_commands_for(selected), key=lambda item: len(item[0]), reverse=True):
        pattern = _pattern(phrase)
        out, count = pattern.subn(replacement, out)
        if count:
            applied.append({"command": phrase, "replacement": replacement, "count": str(count)})
    out = _tidy(out) if applied else text
    return FormatCommandResult(
        text=out,
        enabled=True,
        changed=out != text,
        command_set=selected,
        applied=applied,
    )
