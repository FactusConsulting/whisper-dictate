"""Text injection: Wayland (ydotool/ydotoold) + X11/paste (pynput).

Verbatim move of Dictate's injection/focus methods into a mixin so
Dictate(InjectMixin) keeps identical behaviour (same `self.` state set
in Dictate.__init__; Python MRO resolves the methods unchanged). Not
unit-tested (subprocess/OS-heavy) — verified by import-sanity + the
suite importing Dictate, and smoke-tested on Linux.
"""
from __future__ import annotations

import os

from vp_keymap import _build_ydotool_ops
from vp_ydotool import ydotool_socket_path, ydotoold_ready


_WINDOWS_PASTE_TARGETS = (
    "windows terminal",
    "powershell",
    "command prompt",
    "cmd.exe",
    "claude",
    "codex",
)
_WINDOWS_LAYOUT_SENSITIVE_CHARS = frozenset("'`´^~\"")


class InjectMixin:
    def _capture_target_window(self):
        # Capture the active window at the moment PTT is pressed.
        # CPU transcription takes 4+ seconds; by then focus has drifted.
        # Storing the XID lets _inject() refocus before sending Ctrl+V.
        import subprocess, shutil
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
        import subprocess, shutil
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
        # ydotool type (v1.0.4, no libxkbcommon) silently DROPS non-ASCII
        # that is not covered by the layout keycode map. Surface exactly
        # which characters are lost instead of failing silently.
        dropped = sorted({ch for ch in text
                          if ord(ch) > 127 and ch not in self._keycode_map})
        if dropped:
            print(f"[inject] advarsel: {len(dropped)} tegn uden keycode-map "
                  f"for layout '{self._xkb_layout or '?'}' droppes af "
                  f"ydotool type: {''.join(dropped)}", flush=True)
        for op in _build_ydotool_ops(text, self._keycode_map):
            if not self._try_ydotool(*op):
                return False
        return True

    def _ensure_ydotoold(self) -> None:
        import subprocess, shutil
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
        if r.returncode == 0:
            if self._wait_for_ydotoold():
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
        import subprocess, shutil
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

    def _paste(self, text: str) -> bool:
        try:
            import pyperclip
            from pynput import keyboard

            pyperclip.copy(text)
            self._kb.press(keyboard.Key.ctrl)
            self._kb.press("v")
            self._kb.release("v")
            self._kb.release(keyboard.Key.ctrl)
            return True
        except Exception as e:
            print(f"[inject] paste fejlede: {e}", flush=True)
            return False

    def _inject(self, text: str):
        self._last_inject_strategy = None
        if self.mode == "print":
            self._last_inject_strategy = "print"
            print(f"  (heard) {text}", flush=True)
            return
        on_wayland = bool(os.environ.get('WAYLAND_DISPLAY'))

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

        if on_wayland:
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
            return

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
