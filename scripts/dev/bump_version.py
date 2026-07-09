#!/usr/bin/env python3
"""Bump the whisper-dictate version across all four version files, verified.

Usage:
    python scripts/dev/bump_version.py 1.8.6           # bump repo root files
    python scripts/dev/bump_version.py 1.9.5-rc.1      # prerelease (RC) bump
    python scripts/dev/bump_version.py 1.8.6 --root X  # operate on another tree
    python scripts/dev/bump_version.py --check         # verify consistency only

A prerelease version is `X.Y.Z-rc.N` (N >= 1). Cargo and Nix both accept the
SemVer prerelease suffix as a valid version string, and it is written verbatim
to all four files. Tagging `vX.Y.Z-rc.N` then publishes a GitHub *prerelease*
(see .github/workflows/release.yml) that stable users don't get. See
docs/RELEASING.md for the full RC -> final flow.

The four files that must always agree (a stale Cargo.lock has bitten releases
before — see the 1.8.3 bump):
    VERSION
    src/rust/Cargo.toml        (the [package] version)
    src/rust/Cargo.lock        (the whisper-dictate-app package block)
    nix/package.nix            (the `version ? "..."` default)

Not transactional across files (a crash mid-write can still leave a partial
bump), but it never STARTS writing unless every file matched its expected
pattern: all four replacements are computed and validated up front, then
written, then re-verified — so format drift fails loudly with zero files
touched instead of leaving a half-bumped tree.

Exit codes: 0 = success/consistent, 1 = inconsistent or bad input.
"""
from __future__ import annotations

import argparse
import pathlib
import re
import sys

# A release version is strict semver MAJOR.MINOR.PATCH, optionally followed by a
# release-candidate prerelease suffix `-rc.N` (N >= 1, no leading zero). Per
# SemVer, numeric identifiers must NOT have leading zeros, so each of MAJOR,
# MINOR, PATCH is `0` or a non-zero-led run of digits (`0|[1-9]\d*`). Examples:
#   1.9.5        -> stable release
#   1.9.5-rc.1   -> prerelease (RC); tagging vX.Y.Z-rc.N marks a GitHub prerelease
# Rejected: 1.9.5-rc, 1.9.5-rc.x, 1.9.5-rc.0, 1.9.5-beta.1, v1.9.5, 1.9.5-rc.1.2,
#           01.2.3, 1.02.3, 1.2.03 (leading zeros)
_NUM = r"(?:0|[1-9]\d*)"
_VERSION_RE = re.compile(rf"^{_NUM}\.{_NUM}\.{_NUM}(?:-rc\.[1-9]\d*)?$")

VERSION_FILE = "VERSION"
CARGO_TOML = "Cargo.toml"
CARGO_LOCK = "Cargo.lock"
PACKAGE_NIX = "package.nix"


def _files(root: pathlib.Path) -> dict[str, pathlib.Path]:
    return {
        VERSION_FILE: root / VERSION_FILE,
        CARGO_TOML: root / "src" / "rust" / CARGO_TOML,
        CARGO_LOCK: root / "src" / "rust" / CARGO_LOCK,
        PACKAGE_NIX: root / "nix" / PACKAGE_NIX,
    }


def _read(path: pathlib.Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return None


def read_versions(root: pathlib.Path) -> dict[str, str | None]:
    """The version each of the four files currently declares (None = missing,
    unreadable, or the expected pattern is absent)."""
    files = _files(root)
    out: dict[str, str | None] = {}

    raw = _read(files[VERSION_FILE])
    out[VERSION_FILE] = (raw.strip() or None) if raw is not None else None

    toml = _read(files[CARGO_TOML])
    m = re.search(r'(?m)^version = "([^"]+)"$', toml) if toml else None
    out[CARGO_TOML] = m.group(1) if m else None

    lock = _read(files[CARGO_LOCK])
    m = (re.search(r'name = "whisper-dictate-app"\nversion = "([^"]+)"', lock)
         if lock else None)
    out[CARGO_LOCK] = m.group(1) if m else None

    nix = _read(files[PACKAGE_NIX])
    m = re.search(r'version\s*[?=]\s*"([^"]+)"', nix) if nix else None
    out[PACKAGE_NIX] = m.group(1) if m else None
    return out


def check(root: pathlib.Path) -> int:
    versions = read_versions(root)
    for name, version in versions.items():
        print(f"  {name}: {version or 'NOT FOUND'}")
    distinct = set(versions.values())
    if None in distinct or len(distinct) != 1:
        print("INCONSISTENT version files", file=sys.stderr)
        return 1
    print(f"OK - all four files agree on {distinct.pop()}")
    return 0


def _new_contents(root: pathlib.Path, old: str,
                  new: str) -> dict[pathlib.Path, str] | None:
    """Compute every file's bumped content up front; None if any file does not
    match its expected pattern (in which case NOTHING must be written)."""
    files = _files(root)
    replacements = {
        files[VERSION_FILE]: (old, new + "\n", True),
        files[CARGO_TOML]: (f'version = "{old}"', f'version = "{new}"', False),
        files[CARGO_LOCK]: (
            f'name = "whisper-dictate-app"\nversion = "{old}"',
            f'name = "whisper-dictate-app"\nversion = "{new}"', False),
        files[PACKAGE_NIX]: (
            f'version = "{old}"', f'version = "{new}"', False),
    }
    out: dict[pathlib.Path, str] = {}
    for path, (needle, replacement, whole_file) in replacements.items():
        content = _read(path)
        if content is None or needle not in content:
            print(f"pattern not found in {path} - nothing written",
                  file=sys.stderr)
            return None
        out[path] = (replacement if whole_file
                     else content.replace(needle, replacement, 1))
    return out


def bump(root: pathlib.Path, new: str) -> int:
    if not _VERSION_RE.match(new):
        print(f"not a x.y.z or x.y.z-rc.N version: {new!r}", file=sys.stderr)
        return 1
    if check(root) != 0:
        print("refusing to bump from an inconsistent state", file=sys.stderr)
        return 1
    old = read_versions(root)[VERSION_FILE]
    if old == new:
        print(f"already at {new}")
        return 0

    contents = _new_contents(root, str(old), new)
    if contents is None:
        return 1
    for path, content in contents.items():
        # Developer release tool: --root is by definition the developer's own
        # checkout, so the S2083 "user-controlled path" finding does not apply.
        path.write_text(content, encoding="utf-8")  # NOSONAR

    # Re-verify so silent drift fails loudly instead of shipping a
    # half-bumped tree.
    print(f"bumped {old} -> {new}; verifying:")
    return check(root)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", nargs="?",
                        help="new x.y.z or x.y.z-rc.N version")
    parser.add_argument("--root", default=".", help="repo root (default: cwd)")
    parser.add_argument("--check", action="store_true",
                        help="only verify the four files agree")
    args = parser.parse_args(argv)
    root = pathlib.Path(args.root)
    if args.check or not args.version:
        return check(root)
    return bump(root, args.version)


if __name__ == "__main__":
    sys.exit(main())
