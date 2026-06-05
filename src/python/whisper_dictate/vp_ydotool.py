"""ydotool daemon readiness helpers."""
from __future__ import annotations

import os
import shutil
import socket
import subprocess
import time


def ydotool_socket_path() -> str:
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    return os.environ.get("YDOTOOL_SOCKET") or os.path.join(runtime, ".ydotool_socket")


def ydotoold_ready(path: str | None = None, timeout: float = 1.0) -> bool:
    sock = path or ydotool_socket_path()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if ydotool_debug_ready(sock) or unix_socket_connect_ready(sock):
            return True
        time.sleep(0.05)
    return False


def ydotool_debug_ready(path: str) -> bool:
    if not shutil.which("ydotool"):
        return False
    env = dict(os.environ)
    env["YDOTOOL_SOCKET"] = path
    try:
        result = subprocess.run(
            ["ydotool", "debug"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            env=env,
            timeout=0.5,
        )
        return result.returncode == 0
    except Exception:
        return False


def unix_socket_connect_ready(path: str) -> bool:
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(0.2)
        try:
            sock.connect(path)
            return True
        finally:
            sock.close()
    except OSError:
        return False
