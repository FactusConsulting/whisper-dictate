"""Runtime health checks for Wayland/Linux setup."""
from __future__ import annotations

import glob
import os
import subprocess
import sys
from dataclasses import dataclass
from shutil import which

from vp_ydotool import ydotool_socket_path, ydotoold_ready

try:
    import grp
except ImportError:  # Windows
    grp = None


@dataclass
class Check:
    name: str
    ok: bool
    detail: str
    required: bool = True


def _in_group(name: str) -> bool:
    if grp is None:
        return False
    try:
        gid = grp.getgrnam(name).gr_gid
    except KeyError:
        return False
    return gid in os.getgroups()


def _can_import(name: str) -> bool:
    try:
        __import__(name)
        return True
    except Exception:
        return False


def _event_devices_readable() -> tuple[bool, str]:
    paths = sorted(glob.glob("/dev/input/event*"))
    if not paths:
        return False, "no /dev/input/event* devices found"
    readable = [p for p in paths if os.access(p, os.R_OK)]
    if readable:
        return True, f"{len(readable)}/{len(paths)} readable"
    return False, f"0/{len(paths)} readable; add user to input group and log in again"


def _ydotoold_process_detail(socket_ready: bool) -> tuple[bool, str]:
    if socket_ready:
        return True, "accepting connections"
    try:
        r = subprocess.run(["pgrep", "-x", "ydotoold"], capture_output=True, text=True, timeout=1)
    except Exception as e:
        return False, str(e)
    if r.returncode == 0:
        pids = " ".join(r.stdout.split())
        return False, f"process exists but socket is not accepting connections ({pids})"
    return False, "not running"


def _base_checks(on_linux: bool, on_wayland: bool) -> list[Check]:
    return [
        Check("platform", on_linux, sys.platform, required=False),
        Check("session", on_wayland, "Wayland detected" if on_wayland else "not a Wayland session", required=False),
        Check("python", sys.version_info[:2] >= (3, 10), sys.version.split()[0]),
    ]


def _linux_checks() -> list[Check]:
    checks: list[Check] = []
    checks.append(Check("evdev", _can_import("evdev"), "import evdev"))
    checks.append(Check("ydotool", which("ydotool") is not None, which("ydotool") or "not found"))
    checks.append(Check("ydotoold", which("ydotoold") is not None, which("ydotoold") or "not found"))
    checks.append(Check("input group", _in_group("input"), "current process groups include input" if _in_group("input") else "not in input group"))
    ok, detail = _event_devices_readable()
    checks.append(Check("/dev/input", ok, detail))
    checks.append(Check("XDG_RUNTIME_DIR", bool(os.environ.get("XDG_RUNTIME_DIR")), os.environ.get("XDG_RUNTIME_DIR", "unset"), required=False))
    checks.append(Check("WAYLAND_DISPLAY", bool(os.environ.get("WAYLAND_DISPLAY")), os.environ.get("WAYLAND_DISPLAY", "unset"), required=False))

    sock = ydotool_socket_path()
    socket_ready = ydotoold_ready(sock, timeout=0.6)
    checks.append(Check("ydotool socket", os.path.exists(sock), sock, required=False))
    checks.append(Check("ydotool socket ready", socket_ready, sock))
    process_ok, process_detail = _ydotoold_process_detail(socket_ready)
    checks.append(Check("ydotoold process", process_ok, process_detail))
    return checks


def _print_checks(checks: list[Check]) -> bool:
    failed = False

    for c in checks:
        level = "OK" if c.ok else ("FAIL" if c.required else "WARN")
        print(f"[doctor] {level:<4} {c.name}: {c.detail}", flush=True)
        failed = failed or (c.required and not c.ok)
    return failed


def _print_fix_hints() -> None:
    print("[doctor] Fix hints:", flush=True)
    print("  sudo usermod -aG input $USER  # then log out and back in", flush=True)
    print("  sudo apt install ydotool", flush=True)
    print("  python -m pip install -r requirements-cpu.txt", flush=True)


def run_doctor() -> int:
    on_linux = sys.platform.startswith("linux")
    on_wayland = bool(os.environ.get("WAYLAND_DISPLAY")) or os.environ.get("XDG_SESSION_TYPE") == "wayland"
    checks = _base_checks(on_linux, on_wayland)

    if not on_linux:
        _print_checks(checks)
        return 0

    failed = _print_checks(checks + _linux_checks())
    if failed:
        _print_fix_hints()
    return 1 if failed else 0
