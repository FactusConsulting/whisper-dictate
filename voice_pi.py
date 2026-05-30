#!/usr/bin/env python3
r"""whisper-dictate — all-in-one push-to-talk dictation.

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
  --type          force direct keyboard typing on X11/Windows
  --paste         force clipboard + Ctrl+V on X11/Windows
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

if os.name == "nt":
    for stream_name in ("stdout", "stderr"):
        stream = getattr(sys, stream_name, None)
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            try:
                reconfigure(encoding="utf-8", errors="replace")
            except Exception:
                pass

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

# faster_whisper and numpy are imported lazily so --help and smoke tests stay
# independent of ML/audio/keyboard backends. The CUDA DLL bootstrap above must
# still run BEFORE faster_whisper is first imported, which the lazy import
# preserves.

# --- Module surface re-exports for tests and downstream imports ---------
# The split into vp_cli/vp_transcribe/vp_audio/vp_device/vp_keymap/vp_inject
# keeps this module focused on runtime orchestration. The names below are
# re-exported so existing imports (`from voice_pi import _resolve_device`,
# tests' `voice_pi.build_arg_parser`, etc.) continue to work unchanged.
from vp_cli import (  # noqa: E402
    DEVICE, INJECT_MODE, KEY, LANG, MODEL_NAME, QUIT_COUNT, QUIT_WINDOW_MS,
    VALID_INJECT_MODES,
    _print_effective_config, build_arg_parser,
)
from vp_device import VALID_DEVICES, _resolve_device  # noqa: E402
from vp_inject import InjectMixin  # noqa: E402
from vp_keymap import (  # noqa: E402
    _LANG_TO_XKB, _LAYOUT_KEYCODES, _build_ydotool_ops, _detect_xkb_layout,
)
from vp_metrics import append_jsonl, base_event, compact_text, emit_json  # noqa: E402
from vp_history import append_history  # noqa: E402
from vp_version import VERSION  # noqa: E402
from vp_privacy import apply_local_only_network_lock  # noqa: E402
from vp_config import (  # noqa: E402
    apply_config_to_environ, config_mtime, effective_config, load_config,
)


_ARECORD_DEVICE: str | None = None  # set once at startup

_LAZY_EXPORTS = {
    "vp_audio": (
        "MIN_INPUT_DBFS", "MIN_INPUT_SNR_DB", "TARGET_DBFS",
        "_boost_quiet", "_find_arecord_device", "_looks_like_speech",
        "_noise_snr",
    ),
    "vp_transcribe": (
        "BEAM_SIZE", "CONTEXT_MIN_SECONDS", "HALLUCINATIONS",
        "INITIAL_PROMPT", "SR", "STT_BACKEND", "TEMPERATURES",
        "VALID_STT_BACKENDS", "_transcribe", "_transcribe_detail",
        "is_hallucination", "load_stt_model",
    ),
}
_EXPORT_ALIASES = {"_HALLUCINATIONS": ("vp_transcribe", "HALLUCINATIONS")}


def __getattr__(name: str):
    if name in _EXPORT_ALIASES:
        mod_name, attr = _EXPORT_ALIASES[name]
    else:
        for candidate, names in _LAZY_EXPORTS.items():
            if name in names:
                mod_name, attr = candidate, name
                break
        else:
            raise AttributeError(name)
    module = __import__(mod_name, fromlist=[attr])
    value = getattr(module, attr)
    globals()[name] = value
    return value


def _load_runtime_modules() -> None:
    global np
    global MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS
    global _boost_quiet, _find_arecord_device, _looks_like_speech, _noise_snr
    global BEAM_SIZE, CONTEXT_MIN_SECONDS, _HALLUCINATIONS, INITIAL_PROMPT
    global SR, STT_BACKEND, TEMPERATURES, VALID_STT_BACKENDS
    global _transcribe, _transcribe_detail, is_hallucination, load_stt_model

    import numpy as np  # noqa: F401
    from vp_audio import (
        MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS,
        _boost_quiet, _find_arecord_device, _looks_like_speech, _noise_snr,
    )
    from vp_transcribe import (
        BEAM_SIZE, CONTEXT_MIN_SECONDS,
        HALLUCINATIONS as _HALLUCINATIONS,
        INITIAL_PROMPT, SR, STT_BACKEND, TEMPERATURES, VALID_STT_BACKENDS,
        _transcribe, _transcribe_detail, is_hallucination, load_stt_model,
    )


class Dictate(InjectMixin):
    def __init__(self, model: "WhisperModel", key: str, mode: str,
                 lang: str | None, *, json_output: bool = False,
                 metrics_jsonl: str | None = None, model_name: str = "",
                 device: str = "", compute_type: str = "",
                 model_load_s: float | None = None):
        global _ARECORD_DEVICE
        self.model = model
        self.key = key
        self.mode = mode  # "auto" | "type" | "paste" | "print"
        self.lang = lang  # ISO code, or None for auto-detect
        self.json_output = json_output
        self.metrics_jsonl = metrics_jsonl
        self.model_name = model_name
        self.device = device
        self.compute_type = compute_type
        self.stt_backend = STT_BACKEND
        self._config_mtime = config_mtime()
        self._effective_config = effective_config()
        self.parakeet_min_seconds = float(
            self._effective_config.get("parakeet_min_seconds", "1.5"))
        self.release_tail_ms = int(float(
            self._effective_config.get("release_tail_ms", "200")))
        self.model_load_s = model_load_s
        self._restart_required_reported = False
        self._active_profile_name: str | None = None
        self.frames: list[np.ndarray] = []
        self.recording = False
        self._record_started = 0.0
        self._stream = None
        self._arecord_proc = None
        from pynput import keyboard
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

    def _profiled_config(self, base: dict[str, str]) -> dict[str, str]:
        from vp_profiles import apply_profile_settings

        data = load_config()
        after, profile_name = apply_profile_settings(
            base,
            data.get("profiles", []),
            title=getattr(self, "_inject_target_title", None),
            process=getattr(self, "_inject_target_process", None),
        )
        if profile_name != self._active_profile_name:
            self._active_profile_name = profile_name
            print(
                f"[profile] active: {profile_name or 'default'}",
                flush=True,
            )
        return after

    def _apply_effective_config(self, after: dict[str, str]) -> None:
        restart_keys = {"stt_backend", "model", "parakeet_model", "device", "compute_type", "key"}
        changed_restart = [k for k in sorted(restart_keys) if self._effective_config.get(k) != after.get(k)]
        if changed_restart and not self._restart_required_reported:
            print(
                "[config] updated settings require restart/model reload: "
                + ", ".join(changed_restart),
                flush=True,
            )
            self._restart_required_reported = True

        self.mode = (after.get("inject_mode") or self.mode or "auto").lower()
        self.json_output = (after.get("json_output") or "").lower() not in (
            "", "0", "false", "no", "off")
        self.metrics_jsonl = after.get("metrics_jsonl") or None

        new_lang = after.get("lang") or None
        if new_lang != self.lang:
            self.lang = new_lang
            xkb = _detect_xkb_layout(self.lang) or ''
            self._xkb_layout = xkb
            self._keycode_map = _LAYOUT_KEYCODES.get(xkb, {})
            if self._keycode_map:
                print(f"[inject] keycode map: {xkb} ({len(self._keycode_map)} tegn)", flush=True)

        import vp_audio
        import vp_dictionary
        import vp_transcribe

        vp_audio.TARGET_DBFS = float(after.get("target_dbfs", "-20"))
        vp_audio.MIN_INPUT_DBFS = float(after.get("min_input_dbfs", "-55"))
        vp_audio.MIN_INPUT_SNR_DB = float(after.get("min_snr_db", "6"))

        vp_transcribe.BEAM_SIZE = int(after.get("beam_size", "1"))
        vp_transcribe.TEMPERATURES = vp_transcribe._parse_temperatures(after.get("temperature"))
        vp_transcribe.CONTEXT_MIN_SECONDS = float(after.get("context_min_seconds", "0"))
        self.parakeet_min_seconds = float(after.get("parakeet_min_seconds", "1.5"))
        self.release_tail_ms = int(float(after.get("release_tail_ms", "200")))
        vp_transcribe.VAD_THRESHOLD = float(after.get("vad_threshold", "0.3"))
        vp_transcribe.VAD_MIN_SILENCE_MS = int(after.get("vad_min_silence_ms", "600"))
        vp_transcribe.INITIAL_PROMPT = after.get("initial_prompt") or None
        vp_transcribe.STT_DEBUG = (after.get("stt_debug") or "").lower() not in (
            "", "0", "false", "no", "off")
        vp_dictionary.DICTIONARY = vp_dictionary.load_dictionary()
        self._effective_config = after
        print("[config] reloaded live settings", flush=True)

    def _reload_live_config_if_changed(self) -> None:
        mt = config_mtime()
        if mt <= self._config_mtime:
            return
        self._config_mtime = mt
        apply_config_to_environ()
        self._apply_effective_config(self._profiled_config(effective_config()))

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
        self._reload_live_config_if_changed()
        self._capture_target_window()
        after = self._profiled_config(effective_config())
        if after != self._effective_config:
            self._apply_effective_config(after)
        self.frames = []
        self.recording = True
        self._record_started = time.monotonic()
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
        self._reload_live_config_if_changed()
        tail_s = max(0, self.release_tail_ms) / 1000.0
        if tail_s:
            time.sleep(tail_s)
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
        recording_s = (
            time.monotonic() - self._record_started
            if self._record_started else len(pcm) / SR
        )
        if len(pcm) < SR * 0.3:  # <0.3 s — almost certainly a misfire
            print("  (too short — hold the key while you speak)", flush=True)
            return
        if self.stt_backend == "parakeet" and recording_s < self.parakeet_min_seconds:
            print(
                f"  (too short for Parakeet — speak at least {self.parakeet_min_seconds:.1f}s)",
                flush=True,
            )
            return
        try:
            result = _transcribe_detail(self.model, pcm, self.lang)
            text = result.text
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
        inject_t0 = time.monotonic()
        self._inject(text)
        inject_elapsed_ms = int((time.monotonic() - inject_t0) * 1000)
        event = base_event(
            event="utterance",
            text=text,
            raw_text=result.raw_text or text,
            text_preview=compact_text(text),
            text_chars=len(text),
            recording_s=recording_s,
            audio_duration_s=result.duration_s,
            post_boost_dbfs=result.post_boost_dbfs,
            compute_s=result.compute_s,
            real_time_factor=result.real_time_factor,
            language=result.language or self.lang or "auto",
            language_probability=result.language_probability,
            gate=result.gate,
            model=self.model_name,
            stt_backend=self.stt_backend,
            device=self.device,
            compute_type=self.compute_type,
            model_load_s=self.model_load_s,
            inject_mode=self.mode,
            inject_strategy=getattr(self, "_last_inject_strategy", None),
            inject_elapsed_ms=inject_elapsed_ms,
            target_title=getattr(self, "_inject_target_title", None),
            target_process=getattr(self, "_inject_target_process", None),
            profile=getattr(self, "_active_profile_name", None),
            segments=result.segments,
            dictionary_terms=result.dictionary_terms,
            dictionary_replacements=result.dictionary_replacements,
        )
        append_jsonl(self.metrics_jsonl, event)
        try:
            append_history(event)
        except OSError as e:
            print(f"[history] could not write history: {e}", file=sys.stderr, flush=True)
        if self.json_output:
            emit_json(event)

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

        print(f"whisper-dictate [lang={self.lang or 'auto'}] (evdev). Hold "
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
        if on_wayland and not have_evdev:
            sys.exit("Wayland requires evdev for global hotkeys. "
                     "Run setup.sh again or install requirements-cpu.txt; "
                     "use --doctor for a full health check.")

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
        print(f"whisper-dictate [lang={self.lang or 'auto'}] (pynput). Hold "
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
            ln.join()
        except KeyboardInterrupt:
            pass
        finally:
            ln.stop()
        print("\nbye", flush=True)


if __name__ == "__main__":
    if not os.environ.get("VOICEPI_LAUNCHER_PRINTED_VERSION"):
        print(f"whisper-dictate {VERSION}", flush=True)
    ap = build_arg_parser()
    a = ap.parse_args()
    if a.settings_ui:
        from vp_settings_ui import run_settings_ui
        try:
            raise SystemExit(run_settings_ui())
        except RuntimeError as e:
            ap.error(str(e))
    if a.doctor:
        from vp_doctor import run_doctor
        raise SystemExit(run_doctor())
    if a.benchmark_files:
        from vp_benchmark import run_benchmark
        try:
            run_benchmark(
                a.benchmark_files,
                a.benchmark_backends,
                output_jsonl=a.benchmark_jsonl,
            )
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)
    if a.calibrate_mic is not None or a.calibrate_file:
        from vp_calibration import calibrate_file, calibrate_microphone
        try:
            if a.calibrate_file:
                calibrate_file(a.calibrate_file, as_json=a.json)
            else:
                calibrate_microphone(a.calibrate_mic, as_json=a.json)
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)
    if (a.history_list is not None or a.history_last or
            a.history_copy_last or a.history_reinject_last):
        from vp_history import run_history_command
        try:
            if a.history_last:
                run_history_command("last", as_json=a.json)
            elif a.history_copy_last:
                run_history_command("copy-last")
            elif a.history_reinject_last:
                run_history_command("reinject-last")
            else:
                run_history_command("list", limit=a.history_list, as_json=a.json)
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)
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

    if apply_local_only_network_lock():
        print("[privacy] local-only mode enabled; cloud backends and model downloads are blocked", flush=True)

    _load_runtime_modules()

    if (os.environ.get("VOICEPI_DEBUG") or "").strip().lower() not in (
            "", "0", "false", "no", "off"):
        _print_effective_config(a, dev, ctype)

    try:
        backend = STT_BACKEND
        if backend not in VALID_STT_BACKENDS:
            raise ValueError(
                "invalid VOICEPI_STT_BACKEND="
                f"{backend!r}; expected one of {', '.join(VALID_STT_BACKENDS)}")
    except ValueError as e:
        ap.error(str(e))

    label = "NVIDIA Parakeet" if backend == "parakeet" else "Whisper"
    loaded_model_name = a.model
    if backend == "parakeet":
        from vp_parakeet import resolve_parakeet_model_name
        loaded_model_name = resolve_parakeet_model_name(a.model)
    print(f"loading {label} {loaded_model_name} on {dev} ({ctype})… "
          f"first run downloads the model", flush=True)
    if dev == "cpu":
        print("  note: CPU mode — transcription is slower; large-v3-turbo "
              "(default) is the fastest model", flush=True)
    _t = time.monotonic()
    _model = load_stt_model(a.model, dev, ctype)
    _model_load_s = time.monotonic() - _t
    print(f"model ready in {_model_load_s:.1f}s", flush=True)
    if a.transcribe_file:
        from vp_file_transcribe import (
            print_transcribe_file_result, transcribe_file_event,
        )
        event = transcribe_file_event(
            _model,
            a.transcribe_file,
            lang,
            model_name=loaded_model_name,
            stt_backend=backend,
            device=dev,
            compute_type=ctype,
        )
        print_transcribe_file_result(event, as_json=a.json)
        raise SystemExit(0)
    try:
        Dictate(
            _model, a.key, a.mode, lang,
            json_output=a.json,
            metrics_jsonl=os.environ.get("VOICEPI_METRICS_JSONL"),
            model_name=loaded_model_name,
            device=dev,
            compute_type=ctype,
            model_load_s=_model_load_s,
        ).run()
    except KeyboardInterrupt:
        print("\nbye")
