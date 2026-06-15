from __future__ import annotations

import re
from pathlib import Path


DOC_PATHS = [
    Path("README.md"),
    Path("AGENTS.md"),
    Path("CONTRIBUTING.md"),
    *sorted(Path("docs").glob("*.md")),
]


def _markdown_prose(path: Path) -> str:
    text = path.read_text(encoding="utf-8")
    lines: list[str] = []
    in_fence = False
    for line in text.splitlines():
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        # Ignore inline code, HTML tags, and URLs/links so paths such as
        # `scripts/linux/...` and bundle names keep their exact spelling.
        line = re.sub(r"`[^`]*`", "", line)
        line = re.sub(r"<[^>]+>", "", line)
        line = re.sub(r"https?://\S+", "", line)
        line = re.sub(r"\[[^\]]+\]\([^)]+\)", "", line)
        lines.append(line)
    return "\n".join(lines)


def test_docs_use_canonical_platform_capitalization():
    bad: list[str] = []
    bad_macos = re.compile(r"\b(?:MacOS|Mac OS|OSX|OS X)\b")
    # Match prose "linux" as a standalone word, but ignore filenames/paths and
    # package assets like whisper-dictate-linux-<version>.zip.
    bad_linux = re.compile(r"(?<![A-Za-z0-9_./-])linux(?![A-Za-z0-9_./-])")

    for path in DOC_PATHS:
        prose = _markdown_prose(path)
        for lineno, line in enumerate(prose.splitlines(), start=1):
            if bad_macos.search(line):
                bad.append(f"{path}:{lineno}: use macOS")
            if bad_linux.search(line):
                bad.append(f"{path}:{lineno}: use Linux")

    assert not bad, "Bad platform capitalization:\n" + "\n".join(bad)
