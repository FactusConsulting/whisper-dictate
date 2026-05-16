#!/usr/bin/env python3
r"""voice-pi — all-in-one Windows push-to-talk DICTATION.

Speak prompts instead of typing them. Hold the hotkey, speak softly,
release — the transcribed text is injected into whatever window has
focus (Claude Code, pi, a browser chat box, an editor … anything).
A mic→keyboard, not an AI chat: the AI is whatever app you're in.

Runs ENTIRELY ON WINDOWS, in one process. Mic capture and Whisper
both run here; Whisper uses the local RTX 3080 via native CUDA. There
is no server, no HTTP, no WSL — the old WSL split was pure inertia and
its cross-boundary latency bug class is gone by construction.

Windows setup (native Python — NOT the WSL python):
    py -m venv %USERPROFILE%\voice-pi-venv
    %USERPROFILE%\voice-pi-venv\Scripts\activate
    pip install -r requirements-windows.txt
    python voice_pi.py
First run downloads Whisper large-v3 (~3 GB) into the Windows HF cache.

Hold RIGHT CTRL, speak, release → text appears at your cursor.
  --key f9        use a different hold-to-talk key (ctrl_r, alt_r, f9…)
  --paste         inject via clipboard + Ctrl+V (instant, atomic — no
                  dropped spaces; clobbers the clipboard)
  --no-type       just print what was heard (don't inject — testing)
  --model NAME    Whisper model (default large-v3; env VOICEPI_MODEL)
  --lang CODE     spoken-language hint da/en/de/fr… (default da;
                  env VOICEPI_LANG) — reliable on short/soft speech
  --autodetect    let Whisper guess the language (less reliable)
Keep the TARGET window focused while you speak and ~1-2 s after release.
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
MODEL_NAME = os.environ.get("VOICEPI_MODEL", "large-v3")
# Target loudness (dBFS) quiet input is boosted toward before Whisper
# sees it. Soft voiced speech lands at -35..-45 dBFS where Whisper's
# no-speech gate eats it; normalising to ~-20 recovers it without
# clipping. Lower (e.g. -16) = boost harder.
TARGET_DBFS = float(os.environ.get("VOICEPI_TARGET_DBFS", "-20"))
# Spoken-language hint. Whisper large-v3(-turbo) is multilingual; a
# fixed hint is far more reliable than auto-detect on short/soft
# utterances (and avoids da+English mixing flip-flop). "da", "en",
# "de", "fr", … ; --autodetect sets this to None (Whisper guesses).
LANG = os.environ.get("VOICEPI_LANG", "da")


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


class Dictate:
    def __init__(self, model: WhisperModel, key: str, mode: str,
                 lang: str | None):
        self.model = model
        self.key = key
        self.mode = mode  # "type" | "paste" | "print"
        self.lang = lang  # ISO code, or None for auto-detect
        self.frames: list[np.ndarray] = []
        self.recording = False
        self._stream = None
        self._kb = keyboard.Controller()

    def _cb(self, indata, frames, t, status):
        if self.recording:
            self.frames.append(indata.copy())

    def _start(self):
        if self.recording:
            return
        self.frames = []
        self.recording = True
        self._stream = sd.InputStream(
            samplerate=SR, channels=1, dtype="int16", callback=self._cb
        )
        self._stream.start()
        print("● listening…", flush=True)

    def _inject(self, text: str):
        # Small settle so the PTT key-up is processed and focus is
        # stable on the target window before we emit input.
        time.sleep(0.15)
        if self.mode == "print":
            print(f"  (heard) {text}", flush=True)
            return
        if self.mode == "paste":
            import pyperclip
            pyperclip.copy(text)
            self._kb.press(keyboard.Key.ctrl)
            self._kb.press("v")
            self._kb.release("v")
            self._kb.release(keyboard.Key.ctrl)
            return
        # default: type the characters — universal, works in any focused
        # text input without assuming a paste keybinding. Handles æøå.
        self._kb.type(text)

    def _stop_and_transcribe(self):
        if not self.recording:
            return
        self.recording = False
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

    def run(self):
        target = getattr(keyboard.Key, self.key, None)
        if target is None:
            sys.exit(f"unknown key '{self.key}' (e.g. ctrl_r, alt_r, f9)")
        print(f"voice-pi dictation [lang={self.lang or 'auto'}]. Hold "
              f"[{self.key}] to talk → text is "
              f"{'printed' if self.mode == 'print' else 'typed at your cursor'}."
              f" Press Esc (or Ctrl+C) to quit.")

        def on_press(k):
            if k == keyboard.Key.esc:
                return False  # stops the Listener → guaranteed clean exit
            if k == target:
                self._start()

        def on_release(k):
            if k == target:
                threading.Thread(target=self._stop_and_transcribe,
                                 daemon=True).start()

        ln = keyboard.Listener(on_press=on_press, on_release=on_release)
        ln.start()
        # NOT ln.join(): on Windows the global keyboard hook + a blocked
        # join swallows the console SIGINT, so Ctrl+C never raises and
        # the Ctrl press is eaten by the PTT handler instead. Poll the
        # main thread so KeyboardInterrupt is actually delivered; Esc
        # returns False above and stops the Listener (ln.running→False)
        # as the guaranteed in-app exit that can't clash with Ctrl.
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
                    help="pynput Key name held to talk (ctrl_r, alt_r, f9…)")
    ap.add_argument("--model", default=MODEL_NAME,
                    help="Whisper model (default large-v3; env VOICEPI_MODEL)")
    ap.add_argument("--lang", default=LANG,
                    help="spoken-language hint: da, en, de, fr… "
                         "(default da; env VOICEPI_LANG)")
    ap.add_argument("--autodetect", action="store_true",
                    help="let Whisper auto-detect language (less reliable "
                         "on short/soft speech than a fixed --lang)")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--paste", action="store_const", dest="mode",
                   const="paste", help="inject via clipboard + Ctrl+V")
    g.add_argument("--no-type", action="store_const", dest="mode",
                   const="print", help="just print, don't inject")
    ap.set_defaults(mode="type")
    a = ap.parse_args()
    lang = None if a.autodetect else a.lang

    print(f"loading Whisper {a.model} on the RTX 3080 (CUDA)… "
          f"first run downloads ~3 GB", flush=True)
    _t = time.monotonic()
    _model = WhisperModel(a.model, device="cuda", compute_type="int8_float16")
    print(f"model ready in {time.monotonic() - _t:.1f}s", flush=True)
    try:
        Dictate(_model, a.key, a.mode, lang).run()
    except KeyboardInterrupt:
        print("\nbye")
