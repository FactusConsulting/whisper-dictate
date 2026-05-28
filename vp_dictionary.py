"""Custom vocabulary and deterministic post-transcription replacements."""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _int_env(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        return max(0, int(raw))
    except ValueError:
        print(f"[dictionary] ignoring invalid {name}={raw!r}", flush=True)
        return default


def _default_path() -> Path:
    if os.name == "nt":
        base = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        return Path(base) / "WhisperDictate" / "dictionary.json"
    return Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "whisper-dictate" / "dictionary.json"


def _candidate_paths() -> list[Path]:
    raw = os.environ.get("VOICEPI_DICTIONARY")
    if raw:
        return [Path(p).expanduser() for p in raw.split(os.pathsep) if p.strip()]

    here = Path(__file__).resolve().parent
    return [
        _default_path(),
        here / "dictionary.json",
        here / "dictionary.txt",
    ]


def _dedupe(items: list[str]) -> list[str]:
    seen: set[str] = set()
    out: list[str] = []
    for item in items:
        item = str(item).strip()
        key = item.casefold()
        if item and key not in seen:
            seen.add(key)
            out.append(item)
    return out


def _parse_mapping_line(line: str) -> tuple[str, str] | None:
    for sep in ("=>", "->", "="):
        if sep in line:
            left, right = line.split(sep, 1)
            left = left.strip().strip("\"'")
            right = right.strip().strip("\"'")
            if left and right:
                return left, right
    if ":" in line:
        left, right = line.split(":", 1)
        left = left.strip().strip("\"'")
        right = right.strip().strip("\"'")
        if left and right:
            return left, right
    return None


def _parse_text_config(text: str) -> tuple[list[str], dict[str, str]]:
    terms: list[str] = []
    replacements: dict[str, str] = {}
    section = "terms"

    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        header = line.rstrip(":").strip().lower()
        if header in ("[terms]", "terms"):
            section = "terms"
            continue
        if header in ("[replacements]", "replacements"):
            section = "replacements"
            continue
        if line.startswith("-"):
            line = line[1:].strip()
        if section == "replacements":
            mapping = _parse_mapping_line(line)
            if mapping:
                replacements[mapping[0]] = mapping[1]
            continue
        terms.append(line.strip("\"'"))
    return terms, replacements


def _load_path(path: Path) -> tuple[list[str], dict[str, str]]:
    data = path.read_text(encoding="utf-8")
    if path.suffix.lower() == ".json":
        obj = json.loads(data)
        terms_raw = obj.get("terms", [])
        terms: list[str] = []
        for item in terms_raw:
            if isinstance(item, str):
                terms.append(item)
            elif isinstance(item, dict) and item.get("term"):
                terms.append(str(item["term"]))
        repl_raw = obj.get("replacements", {})
        replacements: dict[str, str] = {}
        if isinstance(repl_raw, dict):
            replacements = {str(k): str(v) for k, v in repl_raw.items()}
        elif isinstance(repl_raw, list):
            for item in repl_raw:
                if isinstance(item, dict) and item.get("from") and item.get("to"):
                    replacements[str(item["from"])] = str(item["to"])
        return terms, replacements
    return _parse_text_config(data)


def dictionary_target_path() -> Path:
    """Return the path managed by the dictionary CLI."""
    paths = _candidate_paths()
    return paths[0] if paths else _default_path()


def _read_dictionary_file(
    path: Path,
) -> tuple[dict[str, object], list[str], dict[str, str]]:
    if not path.exists():
        return {}, [], {}
    if path.suffix.lower() == ".json":
        obj = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(obj, dict):
            raise ValueError("dictionary JSON root must be an object")
        terms, replacements = _load_path(path)
        return obj, terms, replacements
    terms, replacements = _load_path(path)
    return {}, terms, replacements


def _write_dictionary_file(
    path: Path,
    terms: list[str],
    replacements: dict[str, str],
    base: dict[str, object] | None = None,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    obj = dict(base or {})
    obj["terms"] = _dedupe(terms)
    obj["replacements"] = dict(sorted(replacements.items()))
    path.write_text(json.dumps(obj, indent=2, ensure_ascii=False) + "\n",
                    encoding="utf-8")


def ensure_dictionary_file(path: Path | None = None) -> Path:
    path = path or dictionary_target_path()
    if not path.exists():
        _write_dictionary_file(path, [], {})
    return path


def add_dictionary_term(term: str, path: Path | None = None) -> tuple[Path, bool]:
    term = term.strip()
    if not term:
        raise ValueError("dictionary term cannot be empty")
    path = path or dictionary_target_path()
    base, terms, replacements = _read_dictionary_file(path)
    before = {item.casefold() for item in terms}
    added = term.casefold() not in before
    if added:
        terms.append(term)
        _write_dictionary_file(path, terms, replacements, base)
    return path, added


def add_dictionary_replacement(
    mapping: str,
    path: Path | None = None,
) -> tuple[Path, str, str, bool]:
    parsed = _parse_mapping_line(mapping)
    if not parsed:
        raise ValueError("replacement must be FROM=TO")
    src, dst = parsed
    path = path or dictionary_target_path()
    base, terms, replacements = _read_dictionary_file(path)
    changed = replacements.get(src) != dst
    if changed:
        replacements[src] = dst
        _write_dictionary_file(path, terms, replacements, base)
    return path, src, dst, changed


def dictionary_status() -> str:
    paths = _candidate_paths()
    dictionary = DICTIONARY
    enabled = _truthy(os.environ.get("VOICEPI_DICTIONARY_ENABLED", "1"))
    lines = [
        f"enabled: {enabled}",
        f"managed path: {dictionary_target_path()}",
        "configured paths: "
        f"{', '.join(str(p) for p in paths) if paths else '(none)'}",
        "loaded paths: "
        f"{', '.join(str(p) for p in dictionary.paths) if dictionary.paths else '(none)'}",
        f"terms: {len(dictionary.terms)}",
        f"replacements: {len(dictionary.replacements)}",
    ]
    preview = dictionary.terms[:10]
    if preview:
        suffix = " ..." if len(dictionary.terms) > len(preview) else ""
        lines.append(f"term preview: {', '.join(preview)}{suffix}")
    return "\n".join(lines)


def open_dictionary(path: Path | None = None) -> Path:
    path = ensure_dictionary_file(path)
    if os.name == "nt":
        os.startfile(str(path))  # type: ignore[attr-defined]
    elif sys.platform == "darwin":
        subprocess.Popen(["open", str(path)])
    else:
        subprocess.Popen(["xdg-open", str(path)])
    return path


@dataclass
class Dictionary:
    terms: list[str] = field(default_factory=list)
    replacements: dict[str, str] = field(default_factory=dict)
    paths: list[Path] = field(default_factory=list)

    def prompt_terms(self) -> list[str]:
        max_terms = _int_env("VOICEPI_DICTIONARY_MAX_TERMS", 80)
        max_chars = _int_env("VOICEPI_DICTIONARY_PROMPT_CHARS", 1200)
        out: list[str] = []
        chars = 0
        for term in self.terms:
            added = len(term) + (2 if out else 0)
            if len(out) >= max_terms or chars + added > max_chars:
                break
            out.append(term)
            chars += added
        return out

    def build_prompt(self, base_prompt: str | None) -> str | None:
        terms = self.prompt_terms()
        parts = []
        if base_prompt:
            parts.append(base_prompt.strip())
        if terms:
            parts.append("Vocabulary: " + ", ".join(terms))
        return "\n".join(p for p in parts if p) or None

    def apply_replacements(self, text: str) -> tuple[str, list[dict[str, object]]]:
        if not text or not self.replacements:
            return text, []
        changed: list[dict[str, object]] = []
        out = text
        for src, dst in sorted(self.replacements.items(), key=lambda kv: len(kv[0]), reverse=True):
            if not src:
                continue
            pattern = re.compile(rf"(?<!\w){re.escape(src)}(?!\w)", re.IGNORECASE)
            out, count = pattern.subn(dst, out)
            if count:
                changed.append({"from": src, "to": dst, "count": count})
        return out, changed


def load_dictionary() -> Dictionary:
    if not _truthy(os.environ.get("VOICEPI_DICTIONARY_ENABLED", "1")):
        return Dictionary()

    terms: list[str] = []
    replacements: dict[str, str] = {}
    loaded: list[Path] = []
    for path in _candidate_paths():
        if not path.exists():
            continue
        try:
            p_terms, p_replacements = _load_path(path)
        except Exception as e:  # noqa: BLE001 - config errors should not block dictation
            print(f"[dictionary] could not load {path}: {e}", flush=True)
            continue
        terms.extend(p_terms)
        replacements.update(p_replacements)
        loaded.append(path)

    dictionary = Dictionary(_dedupe(terms), replacements, loaded)
    if loaded:
        print(
            f"[dictionary] loaded {len(dictionary.terms)} terms and "
            f"{len(dictionary.replacements)} replacements from "
            f"{', '.join(str(p) for p in loaded)}",
            flush=True,
        )
    return dictionary


DICTIONARY = load_dictionary()
