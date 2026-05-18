#!/usr/bin/env python3
r"""voice-pi — all-in-one push-to-talk DICTATION.

Speak prompts instead of typing them. Hold the hotkey, speak softly,
release — the transcribed text is injected into whatever window has
focus (a terminal, a browser chat box, an editor … anything).
A mic→keyboard, not an AI chat: the "AI" is whatever app you're in.

One process: mic capture and Whisper run together, no server, no
network hop. Whisper runs on your NVIDIA GPU (CUDA) when present and
falls back to CPU otherwise — same code, see --device. Use setup.ps1
(Windows) or setup.sh (Linux) for a one-shot, portable install.

First run downloads the model into the Hugging Face cache (turbo
~1.5 GB; large-v3 ~3 GB).

Hold RIGHT CTRL, speak, release → text appears at your cursor.
  --key f9        use a different hold-to-talk key (ctrl_r, alt_r, f9…)
  --key a+b       chord: hold BOTH keys simultaneously (e.g. shift_r+ctrl_r)
  --paste         inject via clipboard + Ctrl+V on X11/Windows
                  (on Wayland direct evdev keycodes are always used instead)
  --no-type       just print what was heard (don't inject — testing)
  --model NAME    Whisper model (default large-v3-turbo, the fastest;
                  env VOICEPI_MODEL)
  --device D      auto|cuda|cpu (default auto; env VOICEPI_DEVICE)
  --lang CODE     spoken-language hint da/en/de/fr… (env VOICEPI_LANG)
                  omit to let Whisper auto-detect (less reliable on short speech)
  --autodetect    alias for omitting --lang

On Wayland (Ubuntu 26.04), text is injected directly via ydotool:
ASCII via ydotool type, æøå via evdev keycodes (compositor maps them
through the DK XKB layout — no clipboard, no paste shortcut).
Stop it by pressing Esc (or Ctrl+C) — that frees the GPU VRAM.
"""
from __future__ import annotations

import argparse
import glob
import os
import re
import site
import sys
import threading
import time

import numpy as np
import sounddevice as sd
from pynput import keyboard


# --- CUDA runtime DLL bootstrap (Windows) -------------------------------
# ctranslate2 (faster-whisper's backend) needs the CUDA runtime libs
# (cublas/cudnn). On Windows the nvidia-*-cu12 pip wheels drop those
# DLLs in site-packages\nvidia\*\bin, which is NOT on the default DLL
# search path. Mirror what LD_LIBRARY_PATH did in the old WSL build:
# register each nvidia\*\bin dir before faster_whisper is imported.
# Guarded + Windows-only so the file still imports cleanly elsewhere.
if os.name == "nt":
    try:
        for sp in site.getsitepackages():
            for d in sorted({os.path.dirname(p) for p in glob.glob(
                    os.path.join(sp, "nvidia", "*", "bin", "*.dll"))}):
                os.add_dll_directory(d)
                os.environ["PATH"] = d + os.pathsep + os.environ.get("PATH", "")
    except Exception as e:  # noqa: BLE001 — never block startup on this
        print(f"[warn] CUDA DLL bootstrap skipped: {e}", flush=True)

from faster_whisper import WhisperModel  # noqa: E402 — must follow bootstrap

SR = 16000
MODEL_NAME = os.environ.get("VOICEPI_MODEL", "large-v3-turbo")
from vp_device import VALID_DEVICES, _resolve_device  # noqa: E402 - sits next to this script
# Compute device: "auto" tries the GPU (CUDA/NVIDIA) and falls back to
# CPU; force with "cuda" or "cpu". faster-whisper/ctranslate2 only
# accelerate on NVIDIA — an AMD GPU box runs CPU. CPU is usable but
# slow, so the default model is the FASTEST (large-v3-turbo).
DEVICE = os.environ.get("VOICEPI_DEVICE", "auto")
from vp_audio import (  # noqa: E402 - sits next to this script
    TARGET_DBFS, MIN_INPUT_DBFS, MIN_INPUT_SNR_DB,
    _noise_snr, _boost_quiet, _looks_like_speech, _find_arecord_device,
)

# Spoken-language hint. Whisper large-v3(-turbo) is multilingual; a
# fixed hint is far more reliable than auto-detect on short/soft
# utterances (and avoids da+English mixing flip-flop). "da", "en",
# "de", "fr", … ; --autodetect sets this to None (Whisper guesses).
LANG = os.environ.get("VOICEPI_LANG")  # None → Whisper auto-detects
# beam_size=1 is fastest on CPU; raise to 5 for better accuracy at the
# cost of 3-4× slower transcription. VOICEPI_BEAM_SIZE=5 is useful on
# machines without GPU where accuracy matters more than latency.
BEAM_SIZE = int(os.environ.get("VOICEPI_BEAM_SIZE", "1"))
# Optional context hint fed to Whisper before each utterance. Improves
# recognition of domain-specific terms (product names, jargon, names).
# Example: VOICEPI_INITIAL_PROMPT="Winget, whisper-dictate, FactusConsulting"
INITIAL_PROMPT = os.environ.get("VOICEPI_INITIAL_PROMPT") or None


def build_arg_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser()
    ap.add_argument("--key", default="ctrl_r",
                    help="pynput Key name held to talk (ctrl_r, alt_r, f9…) "
                         "or chord: shift_r+ctrl_r")
    ap.add_argument("--model", default=MODEL_NAME,
                    help="Whisper model (default large-v3-turbo, fastest; "
                         "env VOICEPI_MODEL)")
    ap.add_argument("--lang", default=LANG,
                    help="spoken-language hint: da, en, de, fr… "
                         "(env VOICEPI_LANG) — omit to let Whisper auto-detect")
    ap.add_argument("--autodetect", action="store_true",
                    help="explicitly auto-detect language (alias for omitting --lang)")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--paste", action="store_const", dest="mode",
                   const="paste",
                   help="inject via clipboard + Ctrl+V on X11/Windows "
                        "(on Wayland direct evdev keycodes are always used)")
    g.add_argument("--no-type", action="store_const", dest="mode",
                   const="print", help="just print, don't inject")
    ap.add_argument("--device", default=DEVICE, choices=VALID_DEVICES,
                    help="auto|cuda|cpu (default auto; env VOICEPI_DEVICE). "
                         "auto = NVIDIA GPU if present, else CPU")
    ap.set_defaults(mode="type")
    return ap


def _transcribe(model: WhisperModel, pcm: np.ndarray,
                lang: str | None) -> str:
    # pcm: int16 mono @ 16 kHz straight from sounddevice — already the
    # rate/layout Whisper wants, so no WAV round-trip or resample (that
    # whole path died with the server). Just int16 → float32 → boost.
    raw_audio = pcm.reshape(-1).astype(np.float32) / 32768.0
    ok, gate = _looks_like_speech(raw_audio)
    if not ok:
        print(f"[gate] {gate}", flush=True)
        return ""
    print(f"[gate] {gate}", flush=True)
    audio = _boost_quiet(raw_audio)
    dur = len(audio) / SR
    in_dbfs = 20 * np.log10(float(np.sqrt(np.mean(audio**2)) or 1e-9))
    t0 = time.monotonic()
    segments, _ = model.transcribe(
        audio,
        language=lang,  # None → Whisper auto-detects
        initial_prompt=INITIAL_PROMPT,  # domain-specific term hints
        beam_size=BEAM_SIZE,
        temperature=[0.0, 0.2],
        # short turns: don't carry prior text — it makes Whisper
        # hallucinate continuations on near-silent input.
        condition_on_previous_text=False,
        # relaxed gates: defaults drop genuinely-quiet-but-real speech
        # as "no speech"; these let soft voiced speech through.
        no_speech_threshold=0.45,
        log_prob_threshold=-1.0,
        vad_filter=True,
        # threshold 0.3 (vs Silero default 0.5): soft voiced speech sits
        # below 0.5 speech-probability. min_silence keeps natural pauses
        # from splitting a sentence mid-thought.
        vad_parameters=dict(threshold=0.3, min_silence_duration_ms=600),
    )
    # Concatenate with Whisper's OWN spacing. Each segment text already
    # carries a leading space on word boundaries (BPE ▁ tokens); a
    # strip()+" ".join() drops that at segment joins → "hørerdig".
    # Join raw, then collapse whitespace runs to one space.
    text = re.sub(r"\s+", " ", "".join(s.text for s in segments)).strip()
    print(f"[stt] dur={dur:.1f}s post-boost={in_dbfs:.0f}dBFS "
          f"compute={time.monotonic() - t0:.1f}s text={text!r}", flush=True)
    return text


from vp_keymap import (  # noqa: E402 - module sits next to this script
    _LANG_TO_XKB,
    _LAYOUT_KEYCODES,
    _build_ydotool_ops,
    _detect_xkb_layout,
)


_ARECORD_DEVICE: str | None = None  # set once at startup


class Dictate:
    def __init__(self, model: WhisperModel, key: str, mode: str,
                 lang: str | None):
        global _ARECORD_DEVICE
        self.model = model
        self.key = key
        self.mode = mode  # "type" | "paste" | "print"
        self.lang = lang  # ISO code, or None for auto-detect
        self.frames: list[np.ndarray] = []
        self.recording = False
        self._stream = None
        self._arecord_proc = None
        self._kb = keyboard.Controller()
        self._inject_target_xwin: str | None = None   # XID captured at record start
        self._inject_target_title: str | None = None  # window title for debug log
        xkb = _detect_xkb_layout(lang) or ''
        self._xkb_layout = xkb
        self._keycode_map = _LAYOUT_KEYCODES.get(xkb, {})
        if self._keycode_map:
            print(f"[inject] keycode map: {xkb} ({len(self._keycode_map)} tegn)", flush=True)
        elif bool(os.environ.get('WAYLAND_DISPLAY')):
            print(f"[inject] ingen keycode map for layout '{xkb}' — kun ASCII via ydotool type", flush=True)
        if bool(os.environ.get('WAYLAND_DISPLAY')):
            self._ensure_ydotoold()
        if _ARECORD_DEVICE is None:
            _ARECORD_DEVICE = _find_arecord_device()
        if _ARECORD_DEVICE:
            print(f"[audio] using arecord -D {_ARECORD_DEVICE} (PipeWire route)", flush=True)
        else:
            print("[audio] using sounddevice (direct ALSA)", flush=True)

    def _cb(self, indata, frames, t, status):
        if self.recording:
            self.frames.append(indata.copy())

    def _arecord_reader(self, proc):
        # Read raw S16_LE mono 16kHz from arecord stdout into self.frames
        chunk = SR * 2 * 1  # 1 second of S16 mono = SR*2 bytes
        while self.recording:
            data = proc.stdout.read(chunk // 8)  # read ~125ms chunks
            if not data:
                break
            arr = np.frombuffer(data, dtype=np.int16).reshape(-1, 1)
            self.frames.append(arr)

    def _start(self):
        if self.recording:
            return
        self._capture_target_window()
        self.frames = []
        self.recording = True
        if _ARECORD_DEVICE:
            import subprocess
            self._arecord_proc = subprocess.Popen(
                ["arecord", "-D", _ARECORD_DEVICE, "-f", "S16_LE",
                 "-r", str(SR), "-c", "1", "-"],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
            )
            threading.Thread(target=self._arecord_reader,
                             args=(self._arecord_proc,), daemon=True).start()
        else:
            self._stream = sd.InputStream(
                samplerate=SR, channels=1, dtype="int16", callback=self._cb
            )
            self._stream.start()
        print("● listening…", flush=True)

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
            pyperclip.copy(text)
            self._kb.press(keyboard.Key.ctrl)
            self._kb.press("v")
            self._kb.release("v")
            self._kb.release(keyboard.Key.ctrl)
            return
        self._kb.type(text)

    def _stop_and_transcribe(self):
        if not self.recording:
            return
        self.recording = False
        if self._arecord_proc:
            self._arecord_proc.terminate()
            self._arecord_proc.wait()
            self._arecord_proc = None
        if self._stream:
            self._stream.stop()
            self._stream.close()
            self._stream = None
        if not self.frames:
            return
        pcm = np.concatenate(self.frames, axis=0).astype(np.int16)
        if len(pcm) < SR * 0.3:  # <0.3 s — almost certainly a misfire
            print("  (too short — hold the key while you speak)", flush=True)
            return
        try:
            text = _transcribe(self.model, pcm, self.lang)
        except Exception as e:  # noqa: BLE001 — surface any failure
            print(f"  ✗ transcribe error: {e}", flush=True)
            return
        if not text:
            print("  (heard nothing — speak a touch louder / mic closer)",
                  flush=True)
            return
        self._inject(text)

    # pynput key name → evdev key code mapping for common PTT keys
    _EVDEV_MAP = {
        'ctrl_l': 'KEY_LEFTCTRL',   'ctrl_r': 'KEY_RIGHTCTRL',
        'shift_l': 'KEY_LEFTSHIFT', 'shift_r': 'KEY_RIGHTSHIFT',
        'alt_l': 'KEY_LEFTALT',     'alt_r': 'KEY_RIGHTALT',
        'super_l': 'KEY_LEFTMETA',  'super_r': 'KEY_RIGHTMETA',
        **{f'f{i}': f'KEY_F{i}' for i in range(1, 13)},
    }

    def _run_evdev(self, key_names: list[str]):
        # Global hotkey detection via evdev — reads /dev/input/event* directly.
        # Works on pure Wayland where pynput's Xorg backend misses events from
        # Wayland-native windows. Requires user to be in the 'input' group.
        import evdev
        import select

        target_codes: set[int] = set()
        for kn in key_names:
            ecname = self._EVDEV_MAP.get(kn)
            if ecname is None:
                sys.exit(f"unknown key '{kn}' for evdev "
                         f"(supported: {', '.join(self._EVDEV_MAP)})")
            code = getattr(evdev.ecodes, ecname, None)
            if code is None:
                sys.exit(f"evdev has no keycode '{ecname}'")
            target_codes.add(code)

        # Open all input devices that have EV_KEY capability (keyboards)
        devices = []
        for path in evdev.list_devices():
            try:
                d = evdev.InputDevice(path)
                if evdev.ecodes.EV_KEY in d.capabilities():
                    devices.append(d)
            except Exception:
                pass
        if not devices:
            sys.exit("evdev: no keyboard devices found — are you in the 'input' group?")

        pressed: set[int] = set()
        recording = False

        print(f"voice-pi dictation [lang={self.lang or 'auto'}] (evdev). Hold "
              f"[{self.key}] to talk. Ctrl+C to quit.", flush=True)

        try:
            while True:
                r, _, _ = select.select(devices, [], [], 0.5)
                for dev in r:
                    try:
                        events = dev.read()
                    except OSError:
                        continue
                    for ev in events:
                        if ev.type != evdev.ecodes.EV_KEY:
                            continue
                        if ev.code not in target_codes:
                            continue
                        if ev.value == evdev.KeyEvent.key_down:
                            pressed.add(ev.code)
                            if target_codes.issubset(pressed) and not recording:
                                recording = True
                                self._start()
                        elif ev.value == evdev.KeyEvent.key_up:
                            pressed.discard(ev.code)
                            if recording and not target_codes.issubset(pressed):
                                recording = False
                                threading.Thread(
                                    target=self._stop_and_transcribe,
                                    daemon=True).start()
        except KeyboardInterrupt:
            pass
        finally:
            for d in devices:
                try:
                    d.close()
                except Exception:
                    pass
        print("\nbye", flush=True)

    def run(self):
        # Support chord keys: 'shift_r+ctrl_r' means hold both simultaneously.
        # On Wayland: use evdev (reads /dev/input/event* — global, layout-agnostic).
        # On X11: fall back to pynput's xorg backend.
        key_names = [n.strip() for n in self.key.split('+')]

        on_wayland = bool(os.environ.get('WAYLAND_DISPLAY'))
        try:
            import evdev  # noqa: F401
            have_evdev = True
        except ImportError:
            have_evdev = False

        if on_wayland and have_evdev:
            self._run_evdev(key_names)
            return

        # --- pynput fallback (X11 / Windows / macOS) ---
        targets = set()
        for kn in key_names:
            k = getattr(keyboard.Key, kn, None)
            if k is None:
                sys.exit(f"unknown key '{kn}' (e.g. ctrl_r, shift_r, alt_r, f9)")
            targets.add(k)

        pressed: set = set()
        recording = False

        print(f"voice-pi dictation [lang={self.lang or 'auto'}] (pynput). Hold "
              f"[{self.key}] to talk. Esc or Ctrl+C to quit.", flush=True)

        def on_press(k):
            nonlocal recording
            if k == keyboard.Key.esc:
                return False
            pressed.add(k)
            if targets.issubset(pressed) and not recording:
                recording = True
                self._start()

        def on_release(k):
            nonlocal recording
            if k in targets:
                pressed.discard(k)
                if recording and not targets.issubset(pressed):
                    recording = False
                    threading.Thread(target=self._stop_and_transcribe,
                                     daemon=True).start()

        ln = keyboard.Listener(on_press=on_press, on_release=on_release)
        ln.start()
        try:
            while ln.running:
                time.sleep(0.2)
        except KeyboardInterrupt:
            pass
        finally:
            ln.stop()
        print("\nbye", flush=True)


if __name__ == "__main__":
    ap = build_arg_parser()
    a = ap.parse_args()
    lang = None if (a.autodetect or not a.lang) else a.lang

    # Sæt XKB_DEFAULT_LAYOUT fra --lang så ydotool type og evt. auto-startet
    # ydotoold arver det rigtige layout uden manuel konfiguration.
    if lang and not os.environ.get("XKB_DEFAULT_LAYOUT"):
        xkb = _LANG_TO_XKB.get(lang, lang)
        os.environ["XKB_DEFAULT_LAYOUT"] = xkb

    try:
        dev, ctype = _resolve_device(a.device)
    except ValueError as e:
        ap.error(str(e))

    print(f"loading Whisper {a.model} on {dev} ({ctype})… "
          f"first run downloads the model", flush=True)
    if dev == "cpu":
        print("  note: CPU mode — transcription is slower; large-v3-turbo "
              "(default) is the fastest model", flush=True)
    _t = time.monotonic()
    _model = WhisperModel(a.model, device=dev, compute_type=ctype)
    print(f"model ready in {time.monotonic() - _t:.1f}s", flush=True)
    try:
        Dictate(_model, a.key, a.mode, lang).run()
    except KeyboardInterrupt:
        print("\nbye")
