#!/usr/bin/env python3
"""Bump the whisper-dictate version atomically across all four version files.

Usage:
    python scripts/dev/bump_version.py 1.8.6           # bump repo root files
    python scripts/dev/bump_version.py 1.8.6 --root X  # operate on another tree
    python scripts/dev/bump_version.py --check         # verify consistency only

The four files that must always agree (a stale Cargo.lock has bitten releases
before — see the 1.8.3 bump):
    VERSION
    src/rust/Cargo.toml        (the [package] version)
    src/rust/Cargo.lock        (the whisper-dictate-app package block)
    nix/package.nix            (the `version ? "..."` default)

Exit codes: 0 = success/consistent, 1 = inconsistent or bad input.
"""
from __future__ import annotations

import argparse
import pathlib
import re
import sys

_VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")

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


def read_versions(root: pathlib.Path) -> dict[str, str | None]:
    """The version each of the four files currently declares (None = not found)."""
    files = _files(root)
    out: dict[str, str | None] = {}

    out[VERSION_FILE] = (
        files[VERSION_FILE].read_text(encoding="utf-8").strip() or None)

    toml = files[CARGO_TOML].read_text(encoding="utf-8")
    m = re.search(r'(?m)^version = "([^"]+)"$', toml)
    out[CARGO_TOML] = m.group(1) if m else None

    lock = files[CARGO_LOCK].read_text(encoding="utf-8")
    m = re.search(
        r'name = "whisper-dictate-app"\nversion = "([^"]+)"', lock)
    out[CARGO_LOCK] = m.group(1) if m else None

    nix = files[PACKAGE_NIX].read_text(encoding="utf-8")
    m = re.search(r'version \? "([^"]+)"', nix)
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


def bump(root: pathlib.Path, new: str) -> int:
    if not _VERSION_RE.match(new):
        print(f"not a x.y.z version: {new!r}", file=sys.stderr)
        return 1
    if check(root) != 0:
        print("refusing to bump from an inconsistent state", file=sys.stderr)
        return 1
    old = read_versions(root)[VERSION_FILE]
    if old == new:
        print(f"already at {new}")
        return 0

    files = _files(root)
    files[VERSION_FILE].write_text(new + "\n", encoding="utf-8")

    toml = files[CARGO_TOML].read_text(encoding="utf-8")
    files[CARGO_TOML].write_text(
        toml.replace(f'version = "{old}"', f'version = "{new}"', 1),
        encoding="utf-8")

    lock = files[CARGO_LOCK].read_text(encoding="utf-8")
    files[CARGO_LOCK].write_text(
        lock.replace(
            f'name = "whisper-dictate-app"\nversion = "{old}"',
            f'name = "whisper-dictate-app"\nversion = "{new}"', 1),
        encoding="utf-8")

    nix = files[PACKAGE_NIX].read_text(encoding="utf-8")
    files[PACKAGE_NIX].write_text(
        nix.replace(f'version ? "{old}"', f'version ? "{new}"', 1),
        encoding="utf-8")

    # Re-verify so a silent non-match (file drifted from the expected pattern)
    # fails loudly instead of shipping a half-bumped tree.
    print(f"bumped {old} -> {new}; verifying:")
    return check(root)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", nargs="?", help="new x.y.z version")
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
