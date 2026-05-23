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
  --key f9        use a different hold-to-talk key (ctrl_r, alt_r, f9…;
                  env VOICEPI_KEY)
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
Stop it by pressing Esc 3 times in a row (or Ctrl+C) — that frees
the GPU VRAM. Configure with VOICEPI_QUIT_COUNT (0 disables; 1 = legacy).
"""
from __future__ import annotations

import glob
import os
import site
import sys
import threading
import time

import numpy as np


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

# --- Quiet huggingface_hub first-download noise -------------------------
# faster-whisper fetches the model via huggingface_hub on first run. On
# Windows without Developer Mode the cache prints a long symlinks warning,
# and recent HF versions emit an "unauthenticated requests" nag for
# anonymous downloads. Neither is actionable for a public model fetch —
# they just look like errors to new users. Suppress at multiple layers
# (env gates, Python warnings, HF logger level) to cover both emission
# paths across HF versions. Must run BEFORE any HF code imports.
os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
os.environ.setdefault("HF_HUB_VERBOSITY", "error")
import logging  # noqa: E402
import warnings  # noqa: E402
warnings.filterwarnings("ignore", module=r"huggingface_hub.*")
try:
    import huggingface_hub  # noqa: E402, F401 — registers the logger
    logging.getLogger("huggingface_hub").setLevel(logging.ERROR)
except Exception:  # noqa: BLE001 — never block startup on this
    pass

# faster_whisper is imported lazily in __main__ so --help and unit-test
# imports stay light (numpy is the only heavy dep loaded at module-load
# time). The CUDA DLL bootstrap above must still run BEFORE faster_whisper
# is first imported, which the lazy import preserves.

# --- Module surface re-exports for tests and downstream imports ---------
# The split into vp_cli/vp_transcribe/vp_audio/vp_device/vp_keymap/vp_inject
# keeps this module focused on runtime orchestration. The names below are
# re-exported so existing imports (`from voice_pi import _resolve_device`,
# tests' `voice_pi.build_arg_parser`, etc.) continue to work unchanged.
from vp_audio import (  # noqa: E402
    MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS,
    _boost_quiet, _find_arecord_device, _looks_like_speech, _noise_snr,
)
from vp_cli import (  # noqa: E402
    BEAM_SIZE, DEVICE, INJECT_MODE, KEY, LANG, MODEL_NAME, QUIT_COUNT,
    QUIT_WINDOW_MS, VALID_INJECT_MODES,
    _print_effective_config, build_arg_parser,
)
from vp_device import VALID_DEVICES, _resolve_device  # noqa: E402
from vp_inject import InjectMixin  # noqa: E402
from vp_keymap import (  # noqa: E402
    _LANG_TO_XKB, _LAYOUT_KEYCODES, _build_ydotool_ops, _detect_xkb_layout,
)
from vp_transcribe import (  # noqa: E402
    HALLUCINATIONS as _HALLUCINATIONS,
    INITIAL_PROMPT, SR, _transcribe, is_hallucination,
)


_ARECORD_DEVICE: str | None = None  # set once at startup


class Dictate(InjectMixin):
    def __init__(self, model: "WhisperModel", key: str, mode: str,
                 lang: str | None):
        global _ARECORD_DEVICE
        # Lazy: keep argparse/--help working without audio + input backends.
        from pynput import keyboard
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
            import sounddevice as sd
            self._stream = sd.InputStream(
                samplerate=SR, channels=1, dtype="int16", callback=self._cb
            )
            self._stream.start()
        print("● listening…", flush=True)

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
        if is_hallucination(text):
            print(f"  (hallucination filtreret: {text!r})", flush=True)
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
        from pynput import keyboard
        targets = set()
        for kn in key_names:
            k = getattr(keyboard.Key, kn, None)
            if k is None:
                sys.exit(f"unknown key '{kn}' (e.g. ctrl_r, shift_r, alt_r, f9)")
            targets.add(k)

        pressed: set = set()
        recording = False
        esc_count = 0
        esc_last = 0.0

        quit_hint = f"{QUIT_COUNT}× Esc or Ctrl+C" if QUIT_COUNT > 0 else "Ctrl+C"
        print(f"voice-pi dictation [lang={self.lang or 'auto'}] (pynput). Hold "
              f"[{self.key}] to talk. {quit_hint} to quit.", flush=True)

        def on_press(k):
            nonlocal recording, esc_count, esc_last
            if k == keyboard.Key.esc:
                if QUIT_COUNT > 0:
                    now = time.monotonic()
                    if now - esc_last <= QUIT_WINDOW_MS / 1000.0:
                        esc_count += 1
                    else:
                        esc_count = 1
                    esc_last = now
                    if esc_count >= QUIT_COUNT:
                        return False
                return  # never add Esc to the PTT-key set
            esc_count = 0  # any other key resets the consecutive-Esc streak
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

    if (os.environ.get("VOICEPI_DEBUG") or "").strip().lower() not in (
            "", "0", "false", "no", "off"):
        _print_effective_config(a, dev, ctype)

    print(f"loading Whisper {a.model} on {dev} ({ctype})… "
          f"first run downloads the model", flush=True)
    if dev == "cpu":
        print("  note: CPU mode — transcription is slower; large-v3-turbo "
              "(default) is the fastest model", flush=True)
    from faster_whisper import WhisperModel
    _t = time.monotonic()
    _model = WhisperModel(a.model, device=dev, compute_type=ctype)
    print(f"model ready in {time.monotonic() - _t:.1f}s", flush=True)
    try:
        Dictate(_model, a.key, a.mode, lang).run()
    except KeyboardInterrupt:
        print("\nbye")
