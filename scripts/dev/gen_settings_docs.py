#!/usr/bin/env python3
"""Generate the settings reference in docs/CONFIGURATION.md from the schema.

settings_schema.json (src/python/whisper_dictate/settings_schema.json) is the
SINGLE SOURCE OF TRUTH for whisper-dictate runtime settings. This script reads
it and renders a Markdown reference -- one table per category with columns
Key | Env var | Default | Live/Restart | Description -- writing ONLY between the
marker comments in docs/CONFIGURATION.md:

    <!-- BEGIN GENERATED SETTINGS REFERENCE -->
    ... generated tables ...
    <!-- END GENERATED SETTINGS REFERENCE -->

All hand-written prose outside the markers is left untouched. The per-key
`description` text in the schema is reused verbatim, so the docs prose for a key
and the schema description stay a single source.

Usage:
    py -3.12 scripts/dev/gen_settings_docs.py          # rewrite the block in place
    py -3.12 scripts/dev/gen_settings_docs.py --check   # exit 1 if the block is stale

The generator is idempotent: running it twice produces no diff. A drift-guard
test (src/tests/python/test_settings_docs_generated.py) runs `--check` in CI, so
editing the schema without regenerating the docs fails the build.

Exit codes: 0 = wrote / already in sync; 1 = --check found drift; 2 = bad input.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCHEMA_PATH = REPO_ROOT / "src" / "python" / "whisper_dictate" / "settings_schema.json"
DOCS_PATH = REPO_ROOT / "docs" / "CONFIGURATION.md"

BEGIN_MARKER = "<!-- BEGIN GENERATED SETTINGS REFERENCE -->"
END_MARKER = "<!-- END GENERATED SETTINGS REFERENCE -->"


def _die(message: str) -> None:
    """Print a bad-input error to stderr and exit 2 (the documented contract).

    `raise SystemExit("msg")` would exit 1 — the same code as `--check` drift —
    which is what the review flagged. Bad input (unknown category / missing or
    misordered markers) must be distinguishable from drift, so it exits 2.
    """
    print(message, file=sys.stderr)
    raise SystemExit(2)

# Category -> display title. Table order = this dict's insertion order. Every
# `category` value used in the schema must be present here; the generator fails
# loudly (exit 2) on an unknown category so a new bucket can't silently fall out
# of the docs.
CATEGORY_TITLES: dict[str, str] = {
    "core": "Core (the first-time-setup basics)",
    # Wave 8 of #348 dropped the Parakeet backend, so the title collapses
    # to a Whisper-only label.
    "stt-local": "Local speech-to-text (Whisper)",
    "stt-cloud": "Cloud speech-to-text (OpenAI-compatible APIs)",
    "audio": "Audio capture & voice activity",
    "postprocess": "Dictionary & post-processing",
    "injection": "Injection, hotkeys & feedback",
    "diagnostics": "Diagnostics, history & automation",
    "updates": "Update checks",
}


def load_settings() -> list[dict]:
    data = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    return data["settings"]


def _escape_cell(text: str) -> str:
    """Make a value safe for a Markdown table cell."""
    return text.replace("|", "\\|").replace("\n", " ").strip()


def _default_cell(default) -> str:
    if default is None or default == "":
        return "_(unset)_"
    return f"`{_escape_cell(str(default))}`"


def _live_cell(live: bool) -> str:
    return "Live" if live else "Restart"


def render_block(settings: list[dict]) -> str:
    """Render the generated reference (between, not including, the markers)."""
    unknown = sorted({s["category"] for s in settings} - set(CATEGORY_TITLES))
    if unknown:
        _die(
            f"unknown category in schema: {unknown}. Add it to CATEGORY_TITLES "
            f"in {Path(__file__).name}."
        )

    lines: list[str] = []
    lines.append(
        "_Generated from "
        "`src/python/whisper_dictate/settings_schema.json` by "
        "`scripts/dev/gen_settings_docs.py` -- do not edit this block by hand; "
        "regenerate with `py -3.12 scripts/dev/gen_settings_docs.py`._"
    )
    lines.append("")
    lines.append(
        "Every runtime setting, grouped by area. **Live** settings apply on the "
        "next record start/stop; **Restart** settings (backend, model, device, "
        "compute type, hotkey) need the worker restarted. The env var is read at "
        "startup; the same name without the `VOICEPI_` prefix, lower-cased, is "
        "the `config.json` key."
    )
    lines.append("")

    for category, title in CATEGORY_TITLES.items():
        rows = [s for s in settings if s["category"] == category]
        if not rows:
            continue
        basic = sum(1 for s in rows if not s.get("advanced", True))
        suffix = f" -- {basic} basic" if basic else ""
        lines.append(f"### {title}{suffix}")
        lines.append("")
        lines.append("| Key | Env var | Default | Live/Restart | Description |")
        lines.append("|---|---|---|---|---|")
        for s in rows:
            key = _escape_cell(s["key"])
            env = _escape_cell(s["env"])
            default = _default_cell(s.get("default"))
            live = _live_cell(bool(s.get("live", True)))
            desc = _escape_cell(s.get("description", ""))
            lines.append(
                f"| `{key}` | `{env}` | {default} | {live} | {desc} |"
            )
        lines.append("")

    # Single trailing newline inside the block; join without a final blank line.
    return "\n".join(lines).rstrip("\n") + "\n"


def splice(doc: str, block: str) -> str:
    """Return `doc` with the marker block replaced by `block`."""
    start = doc.find(BEGIN_MARKER)
    end = doc.find(END_MARKER)
    if start == -1 or end == -1:
        _die(f"markers {BEGIN_MARKER} / {END_MARKER} not found in {DOCS_PATH}")
    if end < start:
        _die("END marker appears before BEGIN marker")
    head = doc[: start + len(BEGIN_MARKER)]
    tail = doc[end:]
    return f"{head}\n{block}{tail}"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if docs/CONFIGURATION.md would change (no write)",
    )
    args = parser.parse_args(argv)

    settings = load_settings()
    block = render_block(settings)
    doc = DOCS_PATH.read_text(encoding="utf-8")
    updated = splice(doc, block)

    if args.check:
        if updated != doc:
            sys.stderr.write(
                "docs/CONFIGURATION.md is out of sync with settings_schema.json.\n"
                "Regenerate with: py -3.12 scripts/dev/gen_settings_docs.py\n"
            )
            return 1
        return 0

    if updated != doc:
        DOCS_PATH.write_text(updated, encoding="utf-8", newline="\n")
        print(f"updated {DOCS_PATH.relative_to(REPO_ROOT)}")
    else:
        print(f"{DOCS_PATH.relative_to(REPO_ROOT)} already up to date")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
