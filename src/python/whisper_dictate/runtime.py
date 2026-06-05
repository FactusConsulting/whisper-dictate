#!/usr/bin/env python3
r"""whisper-dictate — all-in-one push-to-talk dictation.

Speak prompts instead of typing them. Hold the hotkey, speak softly,
release — the transcribed text is injected into whatever window has
focus (a terminal, a browser chat box, an editor … anything).
A mic→keyboard, not an AI chat: the "AI" is whatever app you're in.

One process: mic capture and Whisper run together, no server, no
network hop. Whisper runs on your NVIDIA GPU (CUDA) when present and
falls back to CPU otherwise — same code, see --device. Use the Rust
whisper-dictate controller for install, settings and runtime management.

First run downloads the model into the Hugging Face cache (turbo
~1.5 GB; large-v3 ~3 GB).

Hold RIGHT CTRL, speak, release → text appears at your cursor.
  --key f9        use a different hold-to-talk key (ctrl_r, alt_r, f9…;
                  env VOICEPI_KEY)
  --key a+b       chord: hold BOTH keys simultaneously (e.g. shift_r+ctrl_r)
  --type          force direct keyboard typing on X11/Windows/Wayland
  --paste         force clipboard + Ctrl+V
  --no-type       just print what was heard (don't inject — testing)
  --model NAME    Whisper model (default large-v3-turbo, the fastest;
                  env VOICEPI_MODEL)
  --device D      auto|cuda|cpu (default auto; env VOICEPI_DEVICE)
  --lang CODE     spoken-language hint da/en/de/fr… (env VOICEPI_LANG)
                  omit to let Whisper auto-detect (less reliable on short speech)
  --autodetect    alias for omitting --lang

On Wayland (Ubuntu 26.04), auto mode uses clipboard + Ctrl+V for
non-ASCII text and direct ydotool injection for plain ASCII. Use --type
to force direct ydotool key injection.
Stop it by pressing Esc 3 times in a row (or Ctrl+C) — that frees
the GPU VRAM. Configure with VOICEPI_QUIT_KEY and VOICEPI_QUIT_COUNT
(0 disables; 1 = legacy).
"""
from __future__ import annotations

import glob
import json
import os
import site
import subprocess
import sys
import threading
import time

def _configure_windows_stdio() -> None:
    if os.name != "nt":
        return
    for stream_name in ("stdout", "stderr"):
        stream = getattr(sys, stream_name, None)
        reconfigure = getattr(stream, "reconfigure", None)
        isatty = getattr(stream, "isatty", None)
        if not callable(reconfigure):
            continue
        try:
            if callable(isatty) and isatty():
                continue
            reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass


_configure_windows_stdio()

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
# The split into focused package modules keeps this module focused on runtime
# orchestration while still exposing the historical runtime surface.
from whisper_dictate.vp_cli import (  # noqa: E402
    DEVICE, INJECT_MODE, KEY, LANG, MODEL_NAME, QUIT_COUNT, QUIT_KEY,
    QUIT_WINDOW_MS,
    VALID_INJECT_MODES,
    _apply_local_only_network_lock, _print_effective_config, build_arg_parser,
)
from whisper_dictate.vp_device import VALID_DEVICES, _resolve_device  # noqa: E402
from whisper_dictate.vp_inject import InjectMixin  # noqa: E402
from whisper_dictate.vp_keymap import (  # noqa: E402
    _LANG_TO_XKB, _LAYOUT_KEYCODES, _build_ydotool_ops, _detect_xkb_layout,
)
from whisper_dictate.vp_version import VERSION  # noqa: E402
from whisper_dictate.vp_postprocess import load_postprocess_settings, postprocess_text  # noqa: E402
from whisper_dictate.vp_formatting import apply_format_commands  # noqa: E402
from whisper_dictate.vp_audio_ducking import AudioDucker, register_active_ducker  # noqa: E402
from whisper_dictate.vp_config import (  # noqa: E402
    apply_config_to_environ, config_mtime, effective_config, load_config,
)


_ARECORD_DEVICE: str | None = None  # set once at startup


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _compact_text(text: str, limit: int = 240) -> str:
    text = " ".join(text.split())
    return text if len(text) <= limit else text[: limit - 3] + "..."


def _base_event(**fields):
    event = {"ts": time.time()}
    event.update(fields)
    return event


def _emit_json(event: dict) -> None:
    print(json.dumps(event, ensure_ascii=False, sort_keys=True), flush=True)


def _rust_helper() -> str | None:
    return os.environ.get("VOICEPI_RUST_INJECTOR")


def _rust_json(command: str, payload: dict, *args: str, timeout: float = 5.0) -> dict | None:
    helper = _rust_helper()
    if not helper:
        return None
    try:
        r = subprocess.run(
            [helper, command, *args],
            input=json.dumps(payload, ensure_ascii=False, sort_keys=True),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout,
            shell=False,
        )
        if r.returncode != 0:
            err = (r.stderr or "").strip()
            if err:
                print(f"[rust:{command}] {err}", file=sys.stderr, flush=True)
            return None
        if r.stderr:
            print(r.stderr, end="", file=sys.stderr, flush=True)
        if not (r.stdout or "").strip():
            return {}
        result = json.loads(r.stdout)
        return result if isinstance(result, dict) else None
    except Exception as e:  # noqa: BLE001 - helper failures should not stop dictation
        print(f"[rust:{command}] {e}", file=sys.stderr, flush=True)
        return None


def _append_jsonl(path: str | None, event: dict) -> None:
    if not path:
        return
    _rust_json("append-jsonl", event, "--path", os.path.expanduser(path))


def _append_history(event: dict) -> None:
    path = event.get("_history_path")
    if path:
        _rust_json("append-history", event, "--path", str(path))
        return
    from whisper_dictate.vp_history import history_enabled, history_path

    if history_enabled():
        _rust_json("append-history", event, "--path", str(history_path()))


def _emit_worker_event(event: str, **fields) -> None:
    if not _truthy(os.environ.get("VOICEPI_WORKER_EVENTS")):
        return
    payload = {"event": event}
    payload.update({key: value for key, value in fields.items() if value is not None})
    _rust_json("worker-event", payload)


def _run_command_hook_and_annotate(event: dict) -> None:
    result = _rust_json(
        "command-hook",
        event,
        timeout=max(
            1.0,
            float(os.environ.get("VOICEPI_COMMAND_HOOK_TIMEOUT_MS") or "2000") / 1000.0 + 1.0,
        ),
    )
    result = result or {
        "enabled": False,
        "command": "",
        "returncode": None,
        "latency_ms": 0,
        "timeout": False,
        "error": None,
    }
    event.update({
        "command_hook_enabled": bool(result.get("enabled", False)),
        "command_hook_command": result.get("command") or None,
        "command_hook_returncode": result.get("returncode"),
        "command_hook_latency_ms": int(result.get("latency_ms") or 0),
        "command_hook_timeout": bool(result.get("timeout", False)),
        "command_hook_error": result.get("error"),
    })


def _apply_profile_settings(base: dict[str, str], profiles, *, title: str | None, process: str | None):
    result = _rust_json("apply-profile", {
        "base": base,
        "profiles": profiles,
        "title": title,
        "process": process,
    })
    if not result:
        return dict(base), None
    config = result.get("config", {})
    if not isinstance(config, dict):
        return dict(base), None
    name = result.get("name")
    return {str(key): str(value) for key, value in config.items()}, str(name) if name else None


def _print_model_capacity(as_json: bool) -> bool:
    helper = _rust_helper()
    if not helper:
        return False
    args = [helper, "model-capacity"]
    if as_json:
        args.append("--json")
    try:
        r = subprocess.run(args, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=5)
    except Exception as e:  # noqa: BLE001
        print(f"[model-capacity] {e}", file=sys.stderr, flush=True)
        return False
    if r.returncode != 0:
        err = (r.stderr or "").strip()
        if err:
            print(f"[model-capacity] {err}", file=sys.stderr, flush=True)
        return False
    print((r.stdout or "").rstrip("\n"), flush=True)
    return True

_LAZY_EXPORTS = {
    "whisper_dictate.vp_audio": (
        "MIN_INPUT_DBFS", "MIN_INPUT_SNR_DB", "TARGET_DBFS",
        "_boost_quiet", "_find_arecord_device", "_looks_like_speech",
        "_noise_snr",
    ),
    "whisper_dictate.vp_transcribe": (
        "BEAM_SIZE", "CONTEXT_MIN_SECONDS", "HALLUCINATIONS",
        "INITIAL_PROMPT", "SR", "STT_BACKEND", "TEMPERATURES",
        "VALID_STT_BACKENDS", "_transcribe", "_transcribe_detail",
        "is_hallucination", "load_stt_model",
    ),
}
_EXPORT_ALIASES = {"_HALLUCINATIONS": ("whisper_dictate.vp_transcribe", "HALLUCINATIONS")}


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
    from whisper_dictate.vp_audio import (
        MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS,
        _boost_quiet, _find_arecord_device, _looks_like_speech, _noise_snr,
    )
    from whisper_dictate.vp_transcribe import (
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
        self.postprocess_settings = load_postprocess_settings()
        self.audio_ducker = register_active_ducker(AudioDucker.from_config())
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
        if self._effective_config.get("xkb_layout"):
            os.environ["VOICEPI_XKB_LAYOUT"] = self._effective_config["xkb_layout"]
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
        data = load_config()
        after, profile_name = _apply_profile_settings(
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
        new_xkb = after.get("xkb_layout") or None
        if new_xkb:
            os.environ["VOICEPI_XKB_LAYOUT"] = new_xkb
        else:
            os.environ.pop("VOICEPI_XKB_LAYOUT", None)
        if new_lang != self.lang or new_xkb != self._effective_config.get("xkb_layout"):
            self.lang = new_lang
            xkb = _detect_xkb_layout(self.lang) or ''
            self._xkb_layout = xkb
            self._keycode_map = _LAYOUT_KEYCODES.get(xkb, {})
            if self._keycode_map:
                print(f"[inject] keycode map: {xkb} ({len(self._keycode_map)} tegn)", flush=True)

        from whisper_dictate import vp_audio
        from whisper_dictate import vp_dictionary
        from whisper_dictate import vp_postprocess
        from whisper_dictate import vp_transcribe
        from whisper_dictate import vp_audio_ducking

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
        self.postprocess_settings = vp_postprocess.load_postprocess_settings()
        self.audio_ducker = vp_audio_ducking.register_active_ducker(
            vp_audio_ducking.AudioDucker.from_config()
        )
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

    def _start_arecord(self) -> None:
        import subprocess
        self._arecord_proc = subprocess.Popen(
            ["arecord", "-D", _ARECORD_DEVICE, "-f", "S16_LE",
             "-r", str(SR), "-c", "1", "-"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
        )
        threading.Thread(
            target=self._arecord_reader,
            args=(self._arecord_proc,),
            daemon=True,
        ).start()

    def _start_sounddevice(self) -> None:
        import sounddevice as sd
        self._stream = sd.InputStream(
            samplerate=SR, channels=1, dtype="int16", callback=self._cb
        )
        self._stream.start()

    def _stop_capture_streams(self) -> None:
        if self._arecord_proc:
            self._arecord_proc.terminate()
            self._arecord_proc.wait()
            self._arecord_proc = None
        if self._stream:
            self._stream.stop()
            self._stream.close()
            self._stream = None

    def _recording_seconds(self, pcm: np.ndarray) -> float:
        if self._record_started:
            return time.monotonic() - self._record_started
        return len(pcm) / SR

    def _should_skip_pcm(self, pcm: np.ndarray, recording_s: float) -> bool:
        if len(pcm) < SR * 0.3:  # <0.3 s — almost certainly a misfire
            print("  (too short — hold the key while you speak)", flush=True)
            return True
        if self.stt_backend == "parakeet" and recording_s < self.parakeet_min_seconds:
            print(
                f"  (too short for Parakeet — speak at least {self.parakeet_min_seconds:.1f}s)",
                flush=True,
            )
            return True
        return False

    def _transcribe_pcm(self, pcm: np.ndarray):
        try:
            result = _transcribe_detail(self.model, pcm, self.lang)
        except Exception as e:  # noqa: BLE001 — surface any failure
            print(f"  ✗ transcribe error: {e}", flush=True)
            return None
        if not result.text:
            print("  (heard nothing — speak a touch louder / mic closer)", flush=True)
            return None
        if is_hallucination(result.text):
            print(f"  (hallucination filtreret: {result.text!r})", flush=True)
            return None
        return result

    def _postprocess_and_format(self, text: str):
        post_result = postprocess_text(text, self.postprocess_settings)
        if post_result.provider == "none" or post_result.mode == "raw":
            print(f"[post] skipped {post_result.mode}/{post_result.provider}", flush=True)
        elif post_result.fallback and post_result.error:
            print(f"[post] fallback after {post_result.latency_ms}ms: {post_result.error}", flush=True)
        elif post_result.changed:
            print(f"[post] {post_result.mode}/{post_result.provider} "
                  f"{post_result.latency_ms}ms text={post_result.text!r}", flush=True)
        else:
            print(f"[post] {post_result.mode}/{post_result.provider} "
                  f"{post_result.latency_ms}ms unchanged", flush=True)
        format_result = apply_format_commands(post_result.text)
        if format_result.changed:
            print(f"[format] {format_result.command_set} commands={format_result.applied}", flush=True)
        return post_result, format_result

    def _utterance_event(
        self,
        *,
        result,
        source_text: str,
        final_text: str,
        recording_s: float,
        inject_elapsed_ms: int,
        post_result,
        format_result,
    ) -> dict:
        return _base_event(
            event="utterance",
            text=final_text,
            dictionary_text=source_text,
            raw_text=result.raw_text or source_text,
            text_preview=_compact_text(final_text),
            text_chars=len(final_text),
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
            post_processor=post_result.provider,
            post_mode=post_result.mode,
            post_model=post_result.model,
            post_latency_ms=post_result.latency_ms,
            post_changed=post_result.changed,
            post_fallback=post_result.fallback,
            post_error=post_result.error or None,
            post_redacted=post_result.redacted,
            post_redactions=post_result.redactions or [],
            format_commands_enabled=format_result.enabled,
            format_commands_set=format_result.command_set,
            format_commands_changed=format_result.changed,
            format_commands_applied=format_result.applied,
        )

    def _record_utterance_event(self, event: dict) -> None:
        _run_command_hook_and_annotate(event)
        if event.get("command_hook_error"):
            print(f"[hook] {event['command_hook_error']}", file=sys.stderr, flush=True)
        _append_jsonl(self.metrics_jsonl, event)
        try:
            _append_history(event)
        except OSError as e:
            print(f"[history] could not write history: {e}", file=sys.stderr, flush=True)
        if self.json_output:
            _emit_json(event)

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
        self.audio_ducker.enter()
        if _ARECORD_DEVICE:
            self._start_arecord()
        else:
            self._start_sounddevice()
        _emit_worker_event("status", state="listening")
        print("● listening…", flush=True)

    def _stop_and_transcribe(self):
        if not self.recording:
            return
        self._reload_live_config_if_changed()
        try:
            tail_s = max(0, self.release_tail_ms) / 1000.0
            if tail_s:
                time.sleep(tail_s)
            self.recording = False
            self._stop_capture_streams()
        finally:
            self.audio_ducker.exit()
        if not self.frames:
            return
        pcm = np.concatenate(self.frames, axis=0).astype(np.int16)
        recording_s = self._recording_seconds(pcm)
        if self._should_skip_pcm(pcm, recording_s):
            return
        result = self._transcribe_pcm(pcm)
        if result is None:
            return
        text = result.text
        post_result, format_result = self._postprocess_and_format(text)
        final_text = format_result.text
        inject_t0 = time.monotonic()
        self._inject(final_text)
        inject_elapsed_ms = int((time.monotonic() - inject_t0) * 1000)
        event = self._utterance_event(
            result=result,
            source_text=text,
            final_text=final_text,
            recording_s=recording_s,
            inject_elapsed_ms=inject_elapsed_ms,
            post_result=post_result,
            format_result=format_result,
        )
        self._record_utterance_event(event)

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

    def _have_evdev(self) -> bool:
        try:
            import evdev  # noqa: F401
            return True
        except ImportError:
            return False

    def _pynput_targets(self, keyboard, key_names: list[str]) -> set:
        targets = set()
        for kn in key_names:
            k = getattr(keyboard.Key, kn, None)
            if k is None:
                sys.exit(f"unknown key '{kn}' (e.g. ctrl_r, shift_r, alt_r, f9)")
            targets.add(k)
        return targets

    def _pynput_quit_key(self, keyboard):
        quit_key = getattr(keyboard.Key, QUIT_KEY, None)
        if quit_key is None and len(QUIT_KEY) == 1:
            quit_key = QUIT_KEY
        if quit_key is None:
            sys.exit(f"unknown quit key '{QUIT_KEY}' (e.g. esc, f12, q)")
        return quit_key

    def _run_pynput(self, key_names: list[str]) -> None:
        # X11 / Windows / macOS fallback.
        from pynput import keyboard

        targets = self._pynput_targets(keyboard, key_names)
        quit_key = self._pynput_quit_key(keyboard)
        pressed: set = set()
        recording = False
        esc_count = 0
        esc_last = 0.0

        quit_hint = f"{QUIT_COUNT}× {QUIT_KEY} or Ctrl+C" if QUIT_COUNT > 0 else "Ctrl+C"
        print(f"whisper-dictate [lang={self.lang or 'auto'}] (pynput). Hold "
              f"[{self.key}] to talk. {quit_hint} to quit.", flush=True)

        def on_press(k):
            nonlocal recording, esc_count, esc_last
            if k == quit_key:
                if QUIT_COUNT > 0:
                    now = time.monotonic()
                    if now - esc_last <= QUIT_WINDOW_MS / 1000.0:
                        esc_count += 1
                    else:
                        esc_count = 1
                    esc_last = now
                    if esc_count >= QUIT_COUNT:
                        return False
                return  # never add the quit key to the PTT-key set
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

    def run(self):
        # Support chord keys: 'shift_r+ctrl_r' means hold both simultaneously.
        # On Wayland: use evdev (reads /dev/input/event* — global, layout-agnostic).
        # On X11: fall back to pynput's xorg backend.
        key_names = [n.strip() for n in self.key.split('+')]
        on_wayland = bool(os.environ.get('WAYLAND_DISPLAY'))

        if on_wayland and self._have_evdev():
            self._run_evdev(key_names)
            return
        if on_wayland:
            sys.exit("Wayland requires evdev for global hotkeys. "
                     "Run whisper-dictate install again or install requirements/cpu.txt; "
                     "use --doctor for a full health check.")

        self._run_pynput(key_names)


def main() -> None:
    if not os.environ.get("VOICEPI_LAUNCHER_PRINTED_VERSION"):
        print(f"whisper-dictate {VERSION}", flush=True)
    ap = build_arg_parser()
    a = ap.parse_args()
    apply_config_to_environ()
    if a.doctor:
        from whisper_dictate.vp_doctor import run_doctor
        raise SystemExit(run_doctor())
    if a.model_capacity:
        if not _print_model_capacity(a.json):
            ap.error("Rust model-capacity helper is not available")
        raise SystemExit(0)
    if a.benchmark_files or a.benchmark_corpus:
        from whisper_dictate.vp_benchmark import run_benchmark
        try:
            run_benchmark(
                a.benchmark_files,
                a.benchmark_backends,
                output_jsonl=a.benchmark_jsonl,
                corpus_manifest=a.benchmark_corpus,
            )
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)
    if a.calibrate_mic is not None or a.calibrate_file:
        from whisper_dictate.vp_calibration import calibrate_file, calibrate_microphone
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
        from whisper_dictate.vp_history import run_history_command
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
    if a.post_process_text is not None:
        from whisper_dictate.vp_postprocess import postprocess_text
        result = postprocess_text(a.post_process_text)
        if result.fallback and result.error:
            print(f"[post] fallback: {result.error}", file=sys.stderr, flush=True)
        print(result.text, flush=True)
        raise SystemExit(0)
    if a.dictionary_suggest:
        from whisper_dictate.vp_dictionary_suggest import print_suggestions, suggest_replacements
        try:
            suggestions = suggest_replacements(
                a.dictionary_suggest,
                min_confidence=a.dictionary_suggest_min_confidence,
            )
            print_suggestions(suggestions, as_json=a.json)
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)
    lang = None if (a.autodetect or not a.lang) else a.lang

    # Sæt XKB_DEFAULT_LAYOUT fra --lang så ydotool type og evt. auto-startet
    # ydotoold arver det rigtige layout uden manuel konfiguration.
    if lang and not os.environ.get("XKB_DEFAULT_LAYOUT"):
        xkb = _LANG_TO_XKB.get(lang, lang)
        os.environ["XKB_DEFAULT_LAYOUT"] = xkb

    if _apply_local_only_network_lock():
        print("[privacy] local-only mode enabled; cloud backends and model downloads are blocked", flush=True)

    _load_runtime_modules()

    try:
        backend = STT_BACKEND
        if backend not in VALID_STT_BACKENDS:
            raise ValueError(
                "invalid VOICEPI_STT_BACKEND="
                f"{backend!r}; expected one of {', '.join(VALID_STT_BACKENDS)}")
    except ValueError as e:
        ap.error(str(e))

    if backend == "openai":
        dev, ctype = "api", "remote"
    else:
        try:
            dev, ctype = _resolve_device(a.device)
        except ValueError as e:
            ap.error(str(e))

    if (os.environ.get("VOICEPI_DEBUG") or "").strip().lower() not in (
            "", "0", "false", "no", "off"):
        _print_effective_config(a, dev, ctype)

    label = (
        "NVIDIA Parakeet" if backend == "parakeet"
        else "External API" if backend == "openai"
        else "Whisper"
    )
    loaded_model_name = a.model
    if backend == "parakeet":
        from whisper_dictate.vp_parakeet import resolve_parakeet_model_name
        loaded_model_name = resolve_parakeet_model_name(a.model)
    elif backend == "openai":
        from whisper_dictate.vp_external_api import load_stt_api_settings
        loaded_model_name = load_stt_api_settings(a.model).model
    if backend == "openai":
        print(f"using {label} {loaded_model_name} via configured API", flush=True)
    else:
        print(f"loading {label} {loaded_model_name} on {dev} ({ctype})… "
              f"first run downloads the model", flush=True)
    _emit_worker_event(
        "status",
        state="loading_model",
        backend=backend,
        model=loaded_model_name,
        device=dev,
        compute_type=ctype,
    )
    if dev == "cpu" and backend != "openai":
        print("  note: CPU mode — transcription is slower; large-v3-turbo "
              "(default) is the fastest model", flush=True)
    _t = time.monotonic()
    try:
        _model = load_stt_model(loaded_model_name, dev, ctype)
    except RuntimeError as e:
        message = str(e)
        _emit_worker_event("error", state="failed", backend=backend, model=loaded_model_name, message=message)
        print(f"  x startup error: {message}", flush=True)
        raise SystemExit(1)
    _model_load_s = time.monotonic() - _t
    _emit_worker_event(
        "status",
        state="ready",
        backend=backend,
        model=loaded_model_name,
        device=dev,
        compute_type=ctype,
        model_load_s=round(_model_load_s, 3),
    )
    if backend == "openai":
        print(f"api ready in {_model_load_s:.1f}s", flush=True)
    else:
        print(f"model ready in {_model_load_s:.1f}s", flush=True)
    if a.transcribe_file:
        from whisper_dictate.vp_file_transcribe import (
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


if __name__ == "__main__":
    main()
