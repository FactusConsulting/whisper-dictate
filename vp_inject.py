"""Text injection: Wayland (ydotool/ydotoold) + X11/paste (pynput).

Verbatim move of Dictate's injection/focus methods into a mixin so
Dictate(InjectMixin) keeps identical behaviour (same `self.` state set
in Dictate.__init__; Python MRO resolves the methods unchanged). Not
unit-tested (subprocess/OS-heavy) — verified by import-sanity + the
suite importing Dictate, and smoke-tested on Linux.
"""
from __future__ import annotations

import os
import time

from vp_keymap import _build_ydotool_ops


class InjectMixin:
    def _capture_target_window(self):
        # Capture the active window at the moment PTT is pressed.
        # CPU transcription takes 4+ seconds; by then focus has drifted.
        # Storing the XID lets _inject() refocus before sending Ctrl+V.
        import subprocess, shutil
        self._inject_target_xwin = None
        self._inject_target_title = None
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
        if subprocess.run(["pgrep", "-x", "ydotoold"], capture_output=True).returncode == 0:
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
            time.sleep(0.8)
            if subprocess.run(["pgrep", "-x", "ydotoold"], capture_output=True).returncode == 0:
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
        time.sleep(0.5)
        print(f"[inject] ydotoold startet (XKB={self._xkb_layout or '?'})", flush=True)

    def _try_ydotool(self, *args: str) -> bool:
        import subprocess, shutil
        if not shutil.which("ydotool"):
            return False
        try:
            r = subprocess.run(["ydotool", *args], capture_output=True, timeout=10)
            if r.returncode != 0:
                err = r.stderr.decode(errors="replace").strip()
                if "ydotool_socket" in err:
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

    def _inject(self, text: str):
        # Settle: let key-up events reach the compositor before injecting.
        time.sleep(0.4)
        if self.mode == "print":
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
            time.sleep(0.1)
        elif target:
            print(f'[inject] → "{preview}"  (target: {target})', flush=True)
        else:
            print(f'[inject] → "{preview}"', flush=True)

        if on_wayland:
            # ASCII via ydotool type, æøå via direkte evdev-keycodes (ydotool key).
            # ydotool type v1.0.4 mangler libxkbcommon og dropper non-ASCII stille;
            # men compositor fortolker KEY_LEFTBRACE→å, KEY_SEMICOLON→ø osv. via
            # XKB dk-layout på ydotoold's uinput-enhed — ingen clipboard nødvendig.
            print(f"[inject] ydotool (direkte)", flush=True)
            if not self._wayland_type(text):
                print("[inject] ydotool fejlede — fallback pynput", flush=True)
                self._kb.type(text)
            return

        # X11 / Windows / macOS: paste via clipboard or type per --paste flag.
        if self.mode == "paste":
            import pyperclip
            from pynput import keyboard
            pyperclip.copy(text)
            self._kb.press(keyboard.Key.ctrl)
            self._kb.press("v")
            self._kb.release("v")
            self._kb.release(keyboard.Key.ctrl)
            return
        self._kb.type(text)
