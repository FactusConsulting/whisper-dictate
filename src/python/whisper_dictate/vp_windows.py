"""Windows-specific helpers shared between injection and window listing.

Extracted from InjectMixin (vp_inject.py) so the window-listing path can
reuse the same ctypes plumbing without duplication.  vp_inject imports
and re-exports the helpers that it previously defined inline, keeping all
existing tests and call-sites unbroken.

Public surface:
  windows_process_name(ctypes, wintypes, pid)  -- process basename for a PID
  list_visible_windows()                        -- JSON-ready window list
  SELF_INJECTION_PROCESSES                      -- set[str]
  SELF_INJECTION_TITLE_RE                       -- re.Pattern
"""
from __future__ import annotations

import os
import re

# ---------------------------------------------------------------------------
# Self-injection guard constants -- shared with vp_inject.InjectMixin
# ---------------------------------------------------------------------------

SELF_INJECTION_PROCESSES: frozenset[str] = frozenset({
    "whisper-dictate",
    "whisper-dictate.exe",
    "whisper_dictate",
    "whisper_dictate.exe",
})

SELF_INJECTION_TITLE_RE: re.Pattern[str] = re.compile(
    r"^whisper-dictate(?:\s+\d.*)?$"
)


def windows_process_name(ctypes, wintypes, pid: int) -> str | None:
    """Return the basename of the executable for Windows process *pid*.

    Pure-ctypes, unit-testable with a stubbed ctypes/wintypes pair.
    Returns ``None`` when the handle cannot be opened.
    """
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    kernel32 = ctypes.windll.kernel32
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return None
    try:
        size = wintypes.DWORD(32768)
        buf = ctypes.create_unicode_buffer(size.value)
        if kernel32.QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size)):
            return os.path.basename(buf.value)
        return str(pid)
    finally:
        kernel32.CloseHandle(handle)


def _is_self_window(title: str, process: str | None) -> bool:
    """Return True when the window belongs to our own process (skip it)."""
    t = " ".join(title.split()).lower()
    if SELF_INJECTION_TITLE_RE.fullmatch(t):
        return True
    if process is None:
        return False
    p = os.path.basename(process.strip()).lower()
    return p in SELF_INJECTION_PROCESSES


def list_visible_windows() -> list[dict]:
    """Enumerate visible top-level windows on Windows via ctypes.

    Returns a list of ``{"title": str, "process": str}`` dicts.  The
    ``title`` field is always non-empty; ``process`` is the executable
    basename when it can be resolved and ``""`` when the process handle
    cannot be opened.  Windows with an empty title or that belong to our
    own process are silently skipped.

    Raises ``RuntimeError`` on non-Windows platforms.
    """
    if os.name != "nt":
        raise RuntimeError("window listing is only supported on Windows")

    import ctypes
    from ctypes import wintypes

    user32 = ctypes.windll.user32
    results: list[dict] = []

    # EnumWindows callback signature: BOOL CALLBACK(HWND, LPARAM)
    # Use wintypes.HWND and wintypes.LPARAM so the callback ABI is
    # correct on both 32-bit and 64-bit Windows (LPARAM is pointer-sized;
    # c_long is always 32 bits and would truncate on 64-bit systems).
    EnumWindowsProc = ctypes.WINFUNCTYPE(
        ctypes.c_bool,
        wintypes.HWND,   # HWND
        wintypes.LPARAM, # LPARAM
    )

    def _cb(hwnd: int, _lparam: int) -> bool:
        try:
            if not user32.IsWindowVisible(hwnd):
                return True  # keep enumerating

            length = user32.GetWindowTextLengthW(hwnd)
            if not length:
                return True

            buf = ctypes.create_unicode_buffer(length + 1)
            user32.GetWindowTextW(hwnd, buf, length + 1)
            title = buf.value
            if not title or not title.strip():
                return True

            pid = wintypes.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            proc: str | None = None
            if pid.value:
                proc = windows_process_name(ctypes, wintypes, pid.value)

            if _is_self_window(title, proc):
                return True

            results.append({
                "title": title,
                "process": proc or "",
            })
        except Exception:  # noqa: BLE001 – never stop enumeration on error
            pass
        return True  # keep enumerating

    user32.EnumWindows(EnumWindowsProc(_cb), 0)
    return results
