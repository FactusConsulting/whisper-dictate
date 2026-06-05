"""Version discovery for installed bundles and development checkouts."""
from __future__ import annotations

import os
import subprocess
from pathlib import Path


def get_version() -> str:
    here = Path(__file__).resolve().parent
    version_file = here / "VERSION"
    try:
        version = version_file.read_text(encoding="utf-8").strip()
        if version:
            return version.removeprefix("v")
    except OSError:
        pass

    try:
        r = subprocess.run(
            ["git", "describe", "--tags", "--always", "--dirty"],
            cwd=here,
            capture_output=True,
            text=True,
            timeout=1,
        )
        if r.returncode == 0:
            version = r.stdout.strip()
            if version:
                return version.removeprefix("v")
    except Exception:
        pass

    return os.environ.get("VOICEPI_VERSION", "unknown").removeprefix("v")


VERSION = get_version()
