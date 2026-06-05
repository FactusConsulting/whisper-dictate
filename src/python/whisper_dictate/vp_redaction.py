"""Local redaction helpers for cloud post-processing."""
from __future__ import annotations

import re
from dataclasses import dataclass, field


@dataclass(frozen=True)
class Redaction:
    placeholder: str
    value: str
    kind: str


@dataclass(frozen=True)
class RedactionResult:
    text: str
    redactions: tuple[Redaction, ...] = field(default_factory=tuple)

    def restore(self, text: str) -> str:
        out = text
        for redaction in self.redactions:
            out = out.replace(redaction.placeholder, redaction.value)
        return out

    def public_summary(self) -> list[dict[str, object]]:
        return [
            {
                "placeholder": redaction.placeholder,
                "kind": redaction.kind,
                "chars": len(redaction.value),
            }
            for redaction in self.redactions
        ]


EMAIL_RE = re.compile(r"\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b", re.IGNORECASE)
API_TOKEN_RE = re.compile(
    r"\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_]{16,}|xox[baprs]-[A-Za-z0-9-]{16,})\b"
)
PHONE_RE = re.compile(r"(?<!\w)(?:\+?\d[\d .()/-]{6,}\d)(?!\w)")


def _term_regex(terms: list[str]) -> re.Pattern[str] | None:
    cleaned = [term.strip() for term in terms if term.strip()]
    if not cleaned:
        return None
    return re.compile(
        r"\b(" + "|".join(re.escape(term) for term in sorted(cleaned, key=len, reverse=True)) + r")\b",
        re.IGNORECASE,
    )


def redact_text(text: str, *, terms: list[str] | None = None) -> RedactionResult:
    redactions: list[Redaction] = []

    def replace(kind: str):
        def inner(match: re.Match[str]) -> str:
            value = match.group(0)
            placeholder = f"[[WD_{kind.upper()}_{len(redactions) + 1}]]"
            redactions.append(Redaction(placeholder=placeholder, value=value, kind=kind))
            return placeholder

        return inner

    out = EMAIL_RE.sub(replace("email"), text)
    out = API_TOKEN_RE.sub(replace("token"), out)
    out = PHONE_RE.sub(replace("phone"), out)
    term_re = _term_regex(terms or [])
    if term_re:
        out = term_re.sub(replace("term"), out)
    return RedactionResult(text=out, redactions=tuple(redactions))
