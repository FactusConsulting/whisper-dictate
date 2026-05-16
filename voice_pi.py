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
                  (on Wayland ydotool type is always used instead)
  --no-type       just print what was heard (don't inject — testing)
  --model NAME    Whisper model (default large-v3-turbo, the fastest;
                  env VOICEPI_MODEL)
  --device D      auto|cuda|cpu (default auto; env VOICEPI_DEVICE)
  --lang CODE     spoken-language hint da/en/de/fr… (env VOICEPI_LANG)
                  omit to let Whisper auto-detect (less reliable on short speech)
  --autodetect    alias for omitting --lang

On Wayland, text is injected via ydotool type. Set VOICEPI_XKB_LAYOUT
(e.g. "dk") so non-ASCII characters (æøå) are typed correctly:
  VOICEPI_XKB_LAYOUT=dk whisper-dictate --key shift_r+ctrl_r --lang da
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
# Compute device: "auto" tries the GPU (CUDA/NVIDIA) and falls back to
# CPU; force with "cuda" or "cpu". faster-whisper/ctranslate2 only
# accelerate on NVIDIA — an AMD GPU box runs CPU. CPU is usable but
# slow, so the default model is the FASTEST (large-v3-turbo).
DEVICE = os.environ.get("VOICEPI_DEVICE", "auto")
# Target loudness (dBFS) quiet input is boosted toward before Whisper
# sees it. Soft voiced speech lands at -35..-45 dBFS where Whisper's
# no-speech gate eats it; normalising to ~-20 recovers it without
# clipping. Lower (e.g. -16) = boost harder.
TARGET_DBFS = float(os.environ.get("VOICEPI_TARGET_DBFS", "-20"))
# Spoken-language hint. Whisper large-v3(-turbo) is multilingual; a
# fixed hint is far more reliable than auto-detect on short/soft
# utterances (and avoids da+English mixing flip-flop). "da", "en",
# "de", "fr", … ; --autodetect sets this to None (Whisper guesses).
LANG = os.environ.get("VOICEPI_LANG")  # None → Whisper auto-detects


def _resolve_device(want: str) -> tuple[str, str]:
    # → (device, compute_type). "auto" uses the GPU if a CUDA/NVIDIA
    # device is present, else CPU. faster-whisper/ctranslate2 only
    # accelerate on NVIDIA, so an AMD-GPU machine resolves to "cpu"
    # (same as a no-GPU box). int8_float16 on GPU, int8 on CPU.
    want = (want or "auto").lower()
    if want in ("cuda", "gpu"):
        return "cuda", "int8_float16"
    if want == "cpu":
        return "cpu", "int8"
    try:
        import ctranslate2
        if ctranslate2.get_cuda_device_count() > 0:
            return "cuda", "int8_float16"
    except Exception:  # noqa: BLE001 — any failure → safe CPU fallback
        pass
    return "cpu", "int8"


def _noise_snr(a: np.ndarray) -> tuple[float, float]:
    # Percentile-based noise-floor / SNR estimate — no VAD, no deps.
    # Frame the RAW (pre-boost) signal into 30 ms windows; the quiet
    # frames between/around words ARE the noise. Noise floor = 10th
    # pct of per-frame RMS (a real mic property in dBFS); SNR = how
    # far the speech (90th pct) sits above it. SNR is gain-invariant
    # so a uniform boost can't flatter it. Few-frame guard avoids
    # log10(0) on near-empty buffers.
    fr = 480  # 30 ms @ 16 kHz
    n = len(a) // fr
    if n < 4:
        return -90.0, 0.0
    frm = a[:n * fr].reshape(n, fr)
    rms = np.sqrt(np.mean(frm.astype(np.float64) ** 2, axis=1))
    lo = float(np.percentile(rms, 10)) or 1e-9
    hi = float(np.percentile(rms, 90)) or 1e-9
    noise_dbfs = 20 * np.log10(lo)
    snr_db = 20 * np.log10(hi / lo)
    return noise_dbfs, snr_db


def _boost_quiet(a: np.ndarray) -> np.ndarray:
    rms = float(np.sqrt(np.mean(a**2)) or 1e-9)
    cur_dbfs = 20 * np.log10(rms)
    gain = 10 ** ((TARGET_DBFS - cur_dbfs) / 20)
    peak = float(np.max(np.abs(a)) or 1e-9)
    gain = min(gain, 0.99 / peak)  # never clip
    noise_dbfs, snr_db = _noise_snr(a)
    print(f"[cap] raw={cur_dbfs:.0f}dBFS peak={peak:.3f} gain={gain:.1f}x "
          f"noise={noise_dbfs:.0f}dBFS snr={snr_db:.0f}dB", flush=True)
    return (a * gain).astype(np.float32)


def _transcribe(model: WhisperModel, pcm: np.ndarray,
                lang: str | None) -> str:
    # pcm: int16 mono @ 16 kHz straight from sounddevice — already the
    # rate/layout Whisper wants, so no WAV round-trip or resample (that
    # whole path died with the server). Just int16 → float32 → boost.
    audio = _boost_quiet(pcm.reshape(-1).astype(np.float32) / 32768.0)
    dur = len(audio) / SR
    in_dbfs = 20 * np.log10(float(np.sqrt(np.mean(audio**2)) or 1e-9))
    t0 = time.monotonic()
    segments, _ = model.transcribe(
        audio,
        language=lang,  # None → Whisper auto-detects
        # greedy decode: for short dictation turns beam width is the
        # dominant latency cost (~5× the GEMM work) and buys almost
        # nothing — soft-speech robustness lives in the encoder, which
        # beam width doesn't touch. One temperature fallback still
        # rescues a genuinely low-SNR quiet utterance.
        beam_size=1,
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


_LANG_TO_XKB = {
    "da": "dk", "de": "de", "fr": "fr", "fi": "fi", "sv": "se",
    "nb": "no", "nn": "no", "nl": "nl", "pl": "pl", "pt": "pt",
    "es": "es", "it": "it", "ru": "ru",
}


def _detect_xkb_layout(lang: str | None = None) -> str | None:
    # Priority: VOICEPI_XKB_LAYOUT > XKB_DEFAULT_LAYOUT > /etc/default/keyboard > lang hint
    for var in ("VOICEPI_XKB_LAYOUT", "XKB_DEFAULT_LAYOUT"):
        v = os.environ.get(var, "").strip()
        if v:
            return v
    try:
        with open("/etc/default/keyboard") as f:
            for line in f:
                m = re.match(r'XKBLAYOUT="?([^"\s]+)"?', line)
                if m:
                    layout = m.group(1)
                    if layout != "us":  # "us" is often wrong on non-US systems
                        return layout
    except FileNotFoundError:
        pass
    # Fall back: derive layout from spoken-language hint (da→dk, de→de, sv→se…)
    if lang and lang in _LANG_TO_XKB:
        return _LANG_TO_XKB[lang]
    return None


def _find_arecord_device() -> str | None:
    # On PipeWire (Ubuntu 24.04+) PortAudio opens ALSA hardware directly and
    # bypasses PipeWire's mixer — the mic reads as silence. arecord with
    # -D pipewire routes through PipeWire correctly.
    import subprocess, shutil, signal
    if not shutil.which("arecord"):
        return None
    for dev in ("pipewire", "default"):
        try:
            # Start without -d (duration), immediately SIGTERM after 0.3s
            p = subprocess.Popen(
                ["arecord", "-D", dev, "-f", "S16_LE", "-r", "16000", "-"],
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            time.sleep(0.3)
            p.send_signal(signal.SIGTERM)
            p.wait(timeout=2)
            stderr = p.stderr.read().decode(errors="replace")
            # SIGTERM gives "Aborted by signal Terminated" — that means it opened OK
            if "Terminated" in stderr or p.returncode in (0, -15, 15):
                return dev
        except Exception:
            pass
    return None


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

    def _try_ydotool(self, *args: str) -> bool:
        import subprocess, shutil
        if not shutil.which("ydotool"):
            return False
        try:
            r = subprocess.run(["ydotool", *args], capture_output=True, timeout=10)
            if r.returncode != 0:
                err = r.stderr.decode(errors="replace").strip()
                if "ydotool_socket" in err and shutil.which("ydotoold"):
                    subprocess.Popen(["ydotoold"],
                                     stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL)
                    time.sleep(0.5)
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
        title = self._inject_target_title or '?'
        if on_wayland and self._restore_target_focus():
            print(f"[inject] → '{title}' (refocused)", flush=True)
            time.sleep(0.1)
        else:
            print(f"[inject] → '{title}'", flush=True)

        if on_wayland:
            # XKB_DEFAULT_LAYOUT er sat i os.environ fra --lang ved startup.
            layout = os.environ.get("XKB_DEFAULT_LAYOUT", "unset")
            print(f"[inject] ydotool type (layout={layout})", flush=True)
            if self._try_ydotool("type", "--", text):
                return
            print("[inject] ydotool type failed — fallback pynput", flush=True)
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
                        "(on Wayland ydotool type is always used)")
    g.add_argument("--no-type", action="store_const", dest="mode",
                   const="print", help="just print, don't inject")
    ap.add_argument("--device", default=DEVICE,
                    help="auto|cuda|cpu (default auto; env VOICEPI_DEVICE). "
                         "auto = NVIDIA GPU if present, else CPU")
    ap.set_defaults(mode="type")
    a = ap.parse_args()
    lang = None if (a.autodetect or not a.lang) else a.lang

    # Sæt XKB_DEFAULT_LAYOUT fra --lang så ydotool type og evt. auto-startet
    # ydotoold arver det rigtige layout uden manuel konfiguration.
    if lang and not os.environ.get("XKB_DEFAULT_LAYOUT"):
        xkb = _LANG_TO_XKB.get(lang, lang)
        os.environ["XKB_DEFAULT_LAYOUT"] = xkb

    dev, ctype = _resolve_device(a.device)

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
