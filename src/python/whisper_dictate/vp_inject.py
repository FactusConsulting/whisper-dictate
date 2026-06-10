"""Text injection: Wayland (ydotool/ydotoold) + X11/paste (pynput).

Verbatim move of Dictate's injection/focus methods into a mixin so
Dictate(InjectMixin) keeps identical behaviour (same `self.` state set
in Dictate.__init__; Python MRO resolves the methods unchanged). Not
unit-tested (subprocess/OS-heavy) — verified by import-sanity + the
suite importing Dictate, and smoke-tested on Linux.
"""
from __future__ import annotations

import os
import re
import shutil
import socket
import subprocess
import sys
import threading
import time

from whisper_dictate.vp_feedback import notify_error

# Seconds to wait before restoring the clipboard after a paste injection.
# The delay is intentional: paste targets (especially on Wayland where
# wl-copy serves content at request time) may read the clipboard lazily.
# Restoring instantly would race against the very paste we just triggered.
_CLIPBOARD_RESTORE_DELAY_S: float = 2.0

# Set to False in tests to prevent background restore threads from spawning.
_CLIPBOARD_RESTORE_ENABLED: bool = True

_WINDOWS_PASTE_TARGETS = (
    "windows terminal",
    "powershell",
    "command prompt",
    "cmd.exe",
    "claude",
    "codex",
)
_WINDOWS_LAYOUT_SENSITIVE_CHARS = frozenset("'`´^~\"")
_WAYLAND_MODIFIER_RELEASES = (
    "29:0",   # KEY_LEFTCTRL
    "97:0",   # KEY_RIGHTCTRL
    "42:0",   # KEY_LEFTSHIFT
    "54:0",   # KEY_RIGHTSHIFT
    "56:0",   # KEY_LEFTALT
    "100:0",  # KEY_RIGHTALT
    "125:0",  # KEY_LEFTMETA
    "126:0",  # KEY_RIGHTMETA
)
_WAYLAND_CTRL_V = ("29:1", "47:1", "47:0", "29:0")
_WAYLAND_CTRL_SHIFT_V = ("29:1", "42:1", "47:1", "47:0", "42:0", "29:0")
_LINUX_TERMINAL_TARGETS = (
    "terminal",
    "ptyxis",
    "kgx",
    "konsole",
    "xterm",
    "alacritty",
    "wezterm",
    "ghostty",
    "kitty",
    "tilix",
    "gnome-console",
    "gnome-terminal",
)
_SELF_INJECTION_PROCESSES = {
    "whisper-dictate",
    "whisper-dictate.exe",
    "whisper_dictate",
    "whisper_dictate.exe",
}
_SELF_INJECTION_TITLE_RE = re.compile(r"^whisper-dictate(?:\s+\d.*)?$")


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
    if not hasattr(socket, "AF_UNIX"):
        return False
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


def _restore_clipboard_after_delay(pyperclip_mod, injected: str, previous: str,
                                   delay_s: float = _CLIPBOARD_RESTORE_DELAY_S) -> None:
    """Restore ``previous`` to the clipboard after ``delay_s`` seconds.

    Runs on a daemon thread spawned by ``_paste``. Restores ONLY if the
    clipboard still holds ``injected`` — never clobber something the user
    copied in the meantime. Clipboard quirks must never break injection, so
    every failure is swallowed. Module-level (not a closure) so tests can
    drive each branch directly with a stubbed pyperclip.
    """
    try:
        time.sleep(delay_s)
        try:
            current = pyperclip_mod.paste()
        except Exception:
            return  # clipboard unreadable — leave it alone
        if current == injected:
            try:
                pyperclip_mod.copy(previous)
            except Exception:
                pass
    except Exception:
        pass  # clipboard quirks must never break injection


class InjectMixin:
    def _capture_target_window(self):
        # Capture the active window at the moment PTT is pressed.
        # CPU transcription takes 4+ seconds; by then focus has drifted.
        # Storing the XID lets _inject() refocus before sending Ctrl+V.
        self._inject_target_xwin = None
        self._inject_target_title = None
        self._inject_target_process = None
        if os.name == "nt":
            self._capture_windows_target()
            return
        if not shutil.which("xdotool"):
            return
        try:
            r = subprocess.run(["xdotool", "getactivewindow"],
                               capture_output=True, timeout=1)
            if r.returncode != 0:
                return
            xwin = r.stdout.decode().strip()
            self._inject_target_xwin = xwin
            rt = subprocess.run(["xdotool", "getwindowname", xwin],
                                capture_output=True, timeout=1)
            if rt.returncode == 0:
                self._inject_target_title = rt.stdout.decode().strip()
        except Exception:
            pass

    def _capture_windows_target(self) -> None:
        try:
            import ctypes
            from ctypes import wintypes

            user32 = ctypes.windll.user32
            hwnd = user32.GetForegroundWindow()
            if not hwnd:
                return

            length = user32.GetWindowTextLengthW(hwnd)
            if length:
                buf = ctypes.create_unicode_buffer(length + 1)
                user32.GetWindowTextW(hwnd, buf, length + 1)
                self._inject_target_title = buf.value

            pid = wintypes.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            if pid.value:
                self._inject_target_process = self._windows_process_name(
                    ctypes, wintypes, pid.value)
        except Exception:
            pass

    def _windows_process_name(self, ctypes, wintypes, pid: int) -> str | None:
        PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
        if not handle:
            return None
        try:
            size = wintypes.DWORD(32768)
            buf = ctypes.create_unicode_buffer(size.value)
            if kernel32.QueryFullProcessImageNameW(
                    handle, 0, buf, ctypes.byref(size)):
                return os.path.basename(buf.value)
            return str(pid)
        finally:
            kernel32.CloseHandle(handle)

    def _restore_target_focus(self) -> bool:
        # For Wayland-native windows (gedit, ghostty…) xdotool finds an XID
        # via getactivewindow but cannot get the title and cannot reliably
        # activate them — windowactivate returns 0 but focuses an XWayland
        # pseudo-window instead, causing ydotool's Ctrl+V to go there.
        # Skip refocus when the title is unknown; Wayland focus does not
        # drift on its own so the target window should still have it.
        if not self._inject_target_xwin or not self._inject_target_title:
            return False
        if not shutil.which("xdotool"):
            return False
        try:
            r = subprocess.run(
                ["xdotool", "windowactivate", "--sync",
                 self._inject_target_xwin],
                capture_output=True, timeout=2)
            return r.returncode == 0
        except Exception:
            return False

    def _wayland_type(self, text: str) -> bool:
        if self._try_rust_inject("type", text):
            return True
        if any(ord(ch) > 127 for ch in text):
            print("[inject] Rust injector required for layout-correct non-ASCII ydotool typing",
                  flush=True)
            return False
        return self._try_ydotool("type", "--", text)

    def _ensure_ydotoold(self) -> None:
        if not shutil.which("ydotoold"):
            return
        if self._wait_for_ydotoold(timeout=0.2):
            return
        # Ryd stale socket så ny instans kan binde
        runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
        sock = os.path.join(runtime, ".ydotool_socket")
        if os.path.exists(sock):
            try:
                os.remove(sock)
            except OSError:
                pass
        # Foretræk systemd-service — den har XKB_DEFAULT_LAYOUT=dk konfigureret
        r = subprocess.run(["systemctl", "--user", "start", "ydotoold.service"],
                           capture_output=True)
        if r.returncode == 0 and self._wait_for_ydotoold():
            print("[inject] ydotoold startet via systemd", flush=True)
            return
        # Fallback: start ydotoold direkte. NB: den autoritative kilde er
        # sessionens XKB-layout, som Mutter applicerer på uinput-enheden —
        # ikke ydotoolds egen env. XKB_DEFAULT_LAYOUT her er kun best-effort
        # for ydotoold-builds der selv læser den; den prioriterede vej er
        # systemd-servicen ovenfor (har XKB konfigureret korrekt).
        env = dict(os.environ)
        if self._xkb_layout:
            env["XKB_DEFAULT_LAYOUT"] = self._xkb_layout
        subprocess.Popen(["ydotoold"],
                         stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL,
                         env=env)
        if self._wait_for_ydotoold():
            print(f"[inject] ydotoold startet (XKB={self._xkb_layout or '?'})", flush=True)
        else:
            print("[inject] ydotoold kunne ikke startes eller socket er ikke klar", flush=True)

    def _wait_for_ydotoold(self, timeout: float = 1.0) -> bool:
        return ydotoold_ready(ydotool_socket_path(), timeout=timeout)

    def _try_ydotool(self, *args: str) -> bool:
        if not shutil.which("ydotool"):
            return False
        try:
            r = subprocess.run(["ydotool", *args], capture_output=True, timeout=10)
            if r.returncode != 0:
                err = r.stderr.decode(errors="replace").strip()
                err_l = err.lower()
                if any(s in err_l for s in (
                        "ydotool_socket", "no such file", "connection refused")):
                    self._ensure_ydotoold()
                    r = subprocess.run(["ydotool", *args],
                                       capture_output=True, timeout=10)
                    err = r.stderr.decode(errors="replace").strip()
                if r.returncode != 0 and err:
                    print(f"[ydotool] {err}", flush=True)
            return r.returncode == 0
        except Exception as e:
            print(f"[ydotool] error: {e}", flush=True)
            return False

    def _try_rust_inject(self, mode: str, text: str = "") -> bool:
        helper = os.environ.get("VOICEPI_RUST_INJECTOR")
        if not helper:
            return False
        args = [
            helper,
            "inject-text",
            "--mode",
            mode,
            "--xkb-layout",
            self._xkb_layout or "",
            "--target-title",
            getattr(self, "_inject_target_title", None) or "",
            "--target-process",
            getattr(self, "_inject_target_process", None) or "",
        ]
        if text:
            args.extend(["--text", text])
        try:
            r = subprocess.run(args, capture_output=True, timeout=10)
            if r.returncode == 0:
                return True
            err = r.stderr.decode(errors="replace").strip()
            if err:
                print(f"[inject] rust injector failed: {err}", flush=True)
            return False
        except Exception as e:
            print(f"[inject] rust injector error: {e}", flush=True)
            return False

    def _target_prefers_paste(self) -> bool:
        if os.name != "nt":
            return False
        target = " ".join(filter(None, (
            getattr(self, "_inject_target_title", None),
            getattr(self, "_inject_target_process", None),
        ))).lower()
        return any(term in target for term in _WINDOWS_PASTE_TARGETS)

    def _text_prefers_paste(self, text: str) -> bool:
        if os.name != "nt":
            return False
        return any(ch in _WINDOWS_LAYOUT_SENSITIVE_CHARS for ch in text)

    def _wayland_text_prefers_paste(self, text: str) -> bool:
        return any(ord(ch) > 127 for ch in text)

    def _wayland_target_prefers_terminal_paste(self) -> bool:
        target = " ".join(filter(None, (
            getattr(self, "_inject_target_title", None),
            getattr(self, "_inject_target_process", None),
        ))).lower()
        if not target:
            # Native Wayland windows often cannot be identified without a
            # compositor extension. Ctrl+Shift+V avoids literal ^V in Linux
            # terminals and is accepted as paste/plain-text paste by common
            # GTK/Electron text widgets.
            return True
        return any(term in target for term in _LINUX_TERMINAL_TARGETS)

    def _target_is_self(self) -> bool:
        process = os.path.basename(
            (getattr(self, "_inject_target_process", None) or "").strip()
        ).lower()
        if process in _SELF_INJECTION_PROCESSES:
            return True
        title = " ".join((getattr(self, "_inject_target_title", None) or "").split()).lower()
        return bool(_SELF_INJECTION_TITLE_RE.fullmatch(title))

    def _wayland_paste_shortcut(self) -> bool:
        if self._try_rust_inject("paste"):
            return True
        # Avoid pynput's keyboard abstraction on Wayland for paste. Sending a
        # deterministic evdev shortcut keeps stale physical modifiers from a PTT
        # chord out of the paste shortcut as much as the compositor allows.
        if not self._try_ydotool("key", *_WAYLAND_MODIFIER_RELEASES):
            return False
        shortcut = (_WAYLAND_CTRL_SHIFT_V
                    if self._wayland_target_prefers_terminal_paste()
                    else _WAYLAND_CTRL_V)
        return self._try_ydotool("key", *shortcut)

    def _paste(self, text: str) -> bool:
        try:
            import pyperclip

            # Save the current clipboard so we can restore it after the paste
            # target has had time to read our injected text.
            previous: str | None = None
            try:
                previous = pyperclip.paste()
            except Exception:
                pass  # clipboard unavailable — nothing to restore

            pyperclip.copy(text)

            if _CLIPBOARD_RESTORE_ENABLED and previous is not None:
                t = threading.Thread(
                    target=_restore_clipboard_after_delay,
                    args=(pyperclip, text, previous),
                    daemon=True,
                )
                t.start()

            if os.environ.get("WAYLAND_DISPLAY") and self._wayland_paste_shortcut():
                return True

            from pynput import keyboard

            for name in (
                    "shift", "shift_l", "shift_r",
                    "alt", "alt_l", "alt_r",
                    "ctrl", "ctrl_l", "ctrl_r",
                    "cmd", "cmd_l", "cmd_r"):
                modifier = getattr(keyboard.Key, name, None)
                if modifier is not None:
                    try:
                        self._kb.release(modifier)
                    except Exception:
                        pass
            self._kb.press(keyboard.Key.ctrl)
            self._kb.press("v")
            self._kb.release("v")
            self._kb.release(keyboard.Key.ctrl)
            return True
        except Exception as e:
            print(f"[inject] paste fejlede: {e}", flush=True)
            return False

    def _inject_log_preview(self, text: str, on_wayland: bool) -> None:
        # CPU transcription takes 4+ seconds — focus has drifted to the
        # terminal by then. Restore the window that was focused when the
        # user pressed the PTT key.
        # Log the TEXT being injected (not a window title). Wayland cannot
        # query/refocus the active window, so the old "→ '?'" looked like
        # a literal question mark was being typed — it was just an unknown
        # target title. Show the target only when actually known.
        preview = " ".join(text.split())
        if len(preview) > 60:
            preview = preview[:57] + "..."
        refocused = on_wayland and self._restore_target_focus()
        target = self._inject_target_title
        if refocused:
            print(f'[inject] → "{preview}"  (refocused: {target})', flush=True)
        elif target:
            print(f'[inject] → "{preview}"  (target: {target})', flush=True)
        else:
            print(f'[inject] → "{preview}"', flush=True)

    def _inject_wayland(self, text: str) -> None:
        mode = self.mode
        if mode == "auto":
            mode = "paste" if self._wayland_text_prefers_paste(text) else "ydotool"
            print(f"[inject] strategy: {mode}", flush=True)
        if mode == "paste":
            self._last_inject_strategy = "paste"
            if self._paste(text):
                return
            print("[inject] paste fejlede — fallback ydotool", flush=True)

        # ASCII via ydotool type. Explicit type also keeps direct key injection
        # available for users who do not want clipboard-based insertion.
        print("[inject] ydotool (direkte)", flush=True)
        self._last_inject_strategy = "ydotool"
        if not self._wayland_type(text):
            print("[inject] ydotool fejlede — fallback pynput", flush=True)
            self._last_inject_strategy = "type-fallback"
            self._kb.type(text)

    def _inject_other(self, text: str) -> None:
        # X11 / Windows / macOS: auto chooses paste for known fragile terminal
        # targets, otherwise direct typing. Explicit --paste/--type override it.
        mode = self.mode
        if mode == "auto":
            mode = "paste" if (
                self._target_prefers_paste() or self._text_prefers_paste(text)
            ) else "type"
            print(f"[inject] strategy: {mode}", flush=True)
        if mode == "paste":
            self._last_inject_strategy = "paste"
            if not self._paste(text):
                self._last_inject_strategy = "type-fallback"
                self._kb.type(text)
            return
        self._last_inject_strategy = "type"
        self._kb.type(text)

    def _inject(self, text: str):
        self._last_inject_strategy = None
        if self.mode == "print":
            self._last_inject_strategy = "print"
            print(f"  (heard) {text}", flush=True)
            return
        on_wayland = bool(os.environ.get('WAYLAND_DISPLAY'))

        self._inject_log_preview(text, on_wayland)

        if self._target_is_self():
            self._last_inject_strategy = "skipped-self"
            print("[inject] skipped self-target", flush=True)
            return

        try:
            if on_wayland:
                self._inject_wayland(text)
            else:
                self._inject_other(text)
        except Exception as exc:
            print(f"[inject] injection failed: {exc}", file=sys.stderr, flush=True)
            notify_error("whisper-dictate", f"Injection failed: {exc}")
