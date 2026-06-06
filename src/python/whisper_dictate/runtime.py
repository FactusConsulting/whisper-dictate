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

import contextlib
import glob
import json
import os
import re
import site
import subprocess
import sys
import threading
import time
import atexit
import wave
from dataclasses import dataclass, field
from pathlib import Path
from shutil import which

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
    VALID_DEVICES, _resolve_device,
    _apply_local_only_network_lock, _print_effective_config, build_arg_parser,
)
from whisper_dictate.vp_inject import InjectMixin, ydotool_socket_path, ydotoold_ready  # noqa: E402
from whisper_dictate.vp_postprocess import load_postprocess_settings, postprocess_text  # noqa: E402
from whisper_dictate.vp_config import (  # noqa: E402
    apply_config_to_environ, config_mtime, effective_config, get_value, load_config,
)


_ARECORD_DEVICE: str | None = None  # set once at startup

_LANG_TO_XKB = {
    "da": "dk", "de": "de", "fr": "fr", "fi": "fi", "sv": "se",
    "nb": "no", "nn": "no", "nl": "nl", "pl": "pl", "pt": "pt",
    "es": "es", "it": "it", "uk": "ua",
}
_SUPPORTED_XKB_LAYOUTS = {"br", "de", "dk", "es", "fi", "no", "pl", "pt", "se", "ua", "us"}


def _normalize_xkb_layout(layout: str | None) -> str | None:
    raw = (layout or "").strip()
    if not raw:
        return None
    mapped = _LANG_TO_XKB.get(raw, raw)
    if mapped in _SUPPORTED_XKB_LAYOUTS:
        return mapped
    return None


def _detect_xkb_layout(lang: str | None = None) -> str | None:
    for var in ("VOICEPI_XKB_LAYOUT", "XKB_DEFAULT_LAYOUT"):
        layout = _normalize_xkb_layout(os.environ.get(var, ""))
        if layout:
            return layout
    try:
        with open("/etc/default/keyboard", encoding="utf-8") as f:
            for line in f:
                match = re.match(r'XKBLAYOUT="?([^"\s]+)"?', line)
                if match:
                    layout = _normalize_xkb_layout(match.group(1))
                    if layout != "us":
                        return layout
    except FileNotFoundError:
        pass
    return _normalize_xkb_layout(lang)


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _float_setting(name: str, default: float, minimum: float, maximum: float) -> float:
    try:
        value = float(get_value(name, str(default)) or default)
    except (TypeError, ValueError):
        value = default
    return min(maximum, max(minimum, value))


@dataclass
class AudioDucker:
    enabled: bool
    target_volume: float
    _sessions: list[tuple[object, float]] = field(default_factory=list)
    _warned: bool = False

    @classmethod
    def from_config(cls) -> "AudioDucker":
        return cls(
            enabled=_truthy(get_value("VOICEPI_AUDIO_DUCKING")),
            target_volume=_float_setting("VOICEPI_AUDIO_DUCKING_LEVEL", 0.25, 0.0, 1.0),
        )

    def enter(self) -> None:
        if not self.enabled or self._sessions:
            return
        if sys.platform != "win32":
            self._warn_once("audio ducking is only implemented on Windows")
            return
        try:
            import comtypes
            from pycaw.pycaw import AudioUtilities, ISimpleAudioVolume

            comtypes.CoInitialize()
            current_pid = os.getpid()
            for session in AudioUtilities.GetAllSessions():
                if session.Process and session.Process.pid == current_pid:
                    continue
                volume = session._ctl.QueryInterface(ISimpleAudioVolume)
                previous = float(volume.GetMasterVolume())
                if previous > self.target_volume:
                    volume.SetMasterVolume(self.target_volume, None)
                    self._sessions.append((volume, previous))
            if self._sessions:
                print(
                    f"[audio-duck] lowered {len(self._sessions)} audio sessions "
                    f"to {self.target_volume:.2f}",
                    flush=True,
                )
        except Exception as exc:
            self._sessions.clear()
            self._warn_once(f"audio ducking unavailable: {exc}")

    def exit(self) -> None:
        if not self._sessions:
            return
        restored = 0
        for volume, previous in reversed(self._sessions):
            try:
                volume.SetMasterVolume(previous, None)
                restored += 1
            except Exception:
                pass
        self._sessions.clear()
        print(f"[audio-duck] restored {restored} audio sessions", flush=True)

    def _warn_once(self, message: str) -> None:
        if self._warned:
            return
        self._warned = True
        print(f"[audio-duck] {message}", flush=True)


_ACTIVE_DUCKERS: list[AudioDucker] = []


def register_active_ducker(ducker: AudioDucker) -> AudioDucker:
    if ducker not in _ACTIVE_DUCKERS:
        _ACTIVE_DUCKERS.append(ducker)
    return ducker


def restore_all_duckers() -> None:
    for ducker in list(_ACTIVE_DUCKERS):
        ducker.exit()


atexit.register(restore_all_duckers)


def get_version() -> str:
    here = Path(__file__).resolve().parent
    version_file = here / "VERSION"
    try:
        version = version_file.read_text(encoding="utf-8").strip()
        if version:
            return version.removeprefix("v")
    except OSError:
        pass

    try:
        r = subprocess.run(
            ["git", "describe", "--tags", "--always", "--dirty"],
            cwd=here,
            capture_output=True,
            text=True,
            timeout=1,
        )
        if r.returncode == 0:
            version = r.stdout.strip()
            if version:
                return version.removeprefix("v")
    except Exception:
        pass

    return os.environ.get("VOICEPI_VERSION", "unknown").removeprefix("v")


VERSION = get_version()


@dataclass(frozen=True)
class FormatCommandResult:
    text: str
    enabled: bool
    changed: bool = False
    command_set: str = "off"
    applied: list[dict[str, str]] = field(default_factory=list)


def _normalize_format_command_set(raw: str | None) -> str:
    raw = (raw or "off").strip().lower()
    if _truthy(raw) and raw not in ("en", "da", "both", "all"):
        return "both"
    if raw == "all":
        return "both"
    if raw in ("en", "da", "both"):
        return raw
    return "off"


def _format_command_set() -> str:
    return _normalize_format_command_set(get_value("VOICEPI_FORMAT_COMMANDS", "off"))


def apply_format_commands(text: str, command_set: str | None = None) -> FormatCommandResult:
    selected = (
        _normalize_format_command_set(command_set)
        if command_set is not None
        else _format_command_set()
    )
    if selected == "off":
        return FormatCommandResult(text=text, enabled=False, command_set="off")
    helper = _rust_helper()
    if not helper:
        raise RuntimeError("Rust format-text helper is not available")
    try:
        r = subprocess.run(
            [
                helper,
                "format-text",
                "--text",
                text,
                "--command-set",
                selected,
            ],
            capture_output=True,
            timeout=5,
            text=True,
        )
    except Exception as e:
        raise RuntimeError(f"Rust format-text helper error: {e}") from e
    if r.returncode != 0:
        err = (r.stderr or "").strip()
        raise RuntimeError(err or "Rust format-text helper failed")
    try:
        payload = json.loads(r.stdout)
    except json.JSONDecodeError as e:
        raise RuntimeError("Rust format-text helper returned invalid JSON") from e
    return FormatCommandResult(
        text=str(payload.get("text", text)),
        enabled=bool(payload.get("enabled", False)),
        changed=bool(payload.get("changed", False)),
        command_set=str(payload.get("command_set", "off")),
        applied=[
            {
                "command": str(item.get("command", "")),
                "replacement": str(item.get("replacement", "")),
                "count": str(item.get("count", "0")),
            }
            for item in payload.get("applied", [])
            if isinstance(item, dict)
        ],
    )


@dataclass
class Check:
    name: str
    ok: bool
    detail: str
    required: bool = True


try:
    import grp
except ImportError:
    grp = None


def _in_group(name: str) -> bool:
    if grp is None:
        return False
    try:
        gid = grp.getgrnam(name).gr_gid
    except KeyError:
        return False
    return gid in os.getgroups()


def _can_import(name: str) -> bool:
    try:
        __import__(name)
        return True
    except Exception:
        return False


def _event_devices_readable() -> tuple[bool, str]:
    paths = sorted(glob.glob("/dev/input/event*"))
    if not paths:
        return False, "no /dev/input/event* devices found"
    readable = [p for p in paths if os.access(p, os.R_OK)]
    if readable:
        return True, f"{len(readable)}/{len(paths)} readable"
    return False, f"0/{len(paths)} readable; add user to input group and log in again"


def _ydotoold_process_detail(socket_ready: bool) -> tuple[bool, str]:
    if socket_ready:
        return True, "accepting connections"
    try:
        r = subprocess.run(["pgrep", "-x", "ydotoold"], capture_output=True, text=True, timeout=1)
    except Exception as e:
        return False, str(e)
    if r.returncode == 0:
        pids = " ".join(r.stdout.split())
        return False, f"process exists but socket is not accepting connections ({pids})"
    return False, "not running"


def _base_checks(on_linux: bool, on_wayland: bool) -> list[Check]:
    return [
        Check("platform", on_linux, sys.platform, required=False),
        Check("session", on_wayland, "Wayland detected" if on_wayland else "not a Wayland session", required=False),
        Check("python", sys.version_info[:2] >= (3, 10), sys.version.split()[0]),
    ]


def _linux_checks() -> list[Check]:
    checks: list[Check] = []
    checks.append(Check("evdev", _can_import("evdev"), "import evdev"))
    checks.append(Check("ydotool", which("ydotool") is not None, which("ydotool") or "not found"))
    checks.append(Check("ydotoold", which("ydotoold") is not None, which("ydotoold") or "not found"))
    checks.append(Check("input group", _in_group("input"), "current process groups include input" if _in_group("input") else "not in input group"))
    ok, detail = _event_devices_readable()
    checks.append(Check("/dev/input", ok, detail))
    checks.append(Check("XDG_RUNTIME_DIR", bool(os.environ.get("XDG_RUNTIME_DIR")), os.environ.get("XDG_RUNTIME_DIR", "unset"), required=False))
    checks.append(Check("WAYLAND_DISPLAY", bool(os.environ.get("WAYLAND_DISPLAY")), os.environ.get("WAYLAND_DISPLAY", "unset"), required=False))

    sock = ydotool_socket_path()
    socket_ready = ydotoold_ready(sock, timeout=0.6)
    checks.append(Check("ydotool socket", os.path.exists(sock), sock, required=False))
    checks.append(Check("ydotool socket ready", socket_ready, sock))
    process_ok, process_detail = _ydotoold_process_detail(socket_ready)
    checks.append(Check("ydotoold process", process_ok, process_detail))
    return checks


def _print_checks(checks: list[Check]) -> bool:
    failed = False

    for c in checks:
        level = "OK" if c.ok else ("FAIL" if c.required else "WARN")
        print(f"[doctor] {level:<4} {c.name}: {c.detail}", flush=True)
        failed = failed or (c.required and not c.ok)
    return failed


def _print_fix_hints() -> None:
    print("[doctor] Fix hints:", flush=True)
    print("  sudo usermod -aG input $USER  # then log out and back in", flush=True)
    print("  sudo apt install ydotool", flush=True)
    print("  python -m pip install -r requirements/cpu.txt", flush=True)


def run_doctor() -> int:
    on_linux = sys.platform.startswith("linux")
    on_wayland = bool(os.environ.get("WAYLAND_DISPLAY")) or os.environ.get("XDG_SESSION_TYPE") == "wayland"
    checks = _base_checks(on_linux, on_wayland)

    if not on_linux:
        _print_checks(checks)
        return 0

    failed = _print_checks(checks + _linux_checks())
    if failed:
        _print_fix_hints()
    return 1 if failed else 0


def _calibration_dbfs(audio) -> float:
    import numpy as np

    rms = float(np.sqrt(np.mean(audio.reshape(-1).astype(np.float32) ** 2)) or 1e-9)
    return 20 * np.log10(rms)


def _calibration_status(raw_dbfs: float, snr_db: float) -> tuple[str, list[str]]:
    warnings_list: list[str] = []
    if raw_dbfs < -55:
        warnings_list.append("input is very quiet")
    elif raw_dbfs < -42:
        warnings_list.append("input is quiet")
    if snr_db < 6:
        warnings_list.append("speech/noise contrast is too low")
    elif snr_db < 15:
        warnings_list.append("speech/noise contrast is marginal")
    if not warnings_list:
        return "pass", []
    if raw_dbfs < -55 or snr_db < 6:
        return "fail", warnings_list
    return "warn", warnings_list


def analyze_calibration_audio(pcm) -> dict:
    import numpy as np
    from whisper_dictate.vp_audio import _noise_snr
    from whisper_dictate.vp_transcribe import SR

    audio = pcm.reshape(-1).astype(np.float32)
    if pcm.dtype.kind in ("i", "u"):
        audio = audio / 32768.0
    raw_dbfs = _calibration_dbfs(audio)
    noise_dbfs, snr_db = _noise_snr(audio)
    peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
    status, warnings_list = _calibration_status(raw_dbfs, snr_db)
    recommended_min_input = min(-35.0, max(-65.0, raw_dbfs - 18.0))
    recommended_min_snr = 6.0 if snr_db < 15 else min(12.0, max(6.0, snr_db - 12.0))
    return {
        "event": "mic_calibration",
        "status": status,
        "warnings": warnings_list,
        "duration_s": len(audio) / SR if len(audio) else 0.0,
        "raw_dbfs": raw_dbfs,
        "noise_dbfs": noise_dbfs,
        "snr_db": snr_db,
        "peak": peak,
        "recommended": {
            "VOICEPI_TARGET_DBFS": "-20",
            "VOICEPI_MIN_INPUT_DBFS": f"{recommended_min_input:.0f}",
            "VOICEPI_MIN_SNR_DB": f"{recommended_min_snr:.0f}",
        },
    }


def record_calibration_audio(seconds: float):
    import numpy as np
    import sounddevice as sd
    from whisper_dictate.vp_transcribe import SR

    seconds = max(1.0, float(seconds))
    print(f"[calibrate] speak normally for {seconds:.1f}s...", flush=True)
    audio = sd.rec(int(seconds * SR), samplerate=SR, channels=1, dtype="int16")
    sd.wait()
    return audio.astype(np.int16)


def print_calibration_result(result: dict, *, as_json: bool = False) -> None:
    if as_json:
        print(json.dumps(result, ensure_ascii=False, separators=(",", ":")),
              flush=True)
        return
    print(f"[calibrate] status={result['status']}", flush=True)
    print(
        "[calibrate] "
        f"raw={result['raw_dbfs']:.0f}dBFS "
        f"noise={result['noise_dbfs']:.0f}dBFS "
        f"snr={result['snr_db']:.0f}dB "
        f"peak={result['peak']:.3f}",
        flush=True,
    )
    for warning in result["warnings"]:
        print(f"[calibrate] warning: {warning}", flush=True)
    rec = result["recommended"]
    print("[calibrate] recommended settings:", flush=True)
    for key, value in rec.items():
        print(f"  {key}={value}", flush=True)


def calibrate_microphone(seconds: float, *, as_json: bool = False) -> dict:
    pcm = record_calibration_audio(seconds)
    result = analyze_calibration_audio(pcm)
    print_calibration_result(result, as_json=as_json)
    return result


def calibrate_file(path: str, *, as_json: bool = False) -> dict:
    t0 = time.monotonic()
    pcm = load_audio_file(path)
    result = analyze_calibration_audio(pcm)
    result["source_file"] = path
    result["decode_s"] = time.monotonic() - t0
    print_calibration_result(result, as_json=as_json)
    return result


def _mono_float_to_int16(audio):
    import numpy as np

    audio = np.clip(audio.reshape(-1), -1.0, 1.0)
    return (audio * 32767.0).astype(np.int16).reshape(-1, 1)


def _resample_mono(audio, source_rate: int):
    import numpy as np
    from whisper_dictate.vp_transcribe import SR

    if source_rate == SR:
        return audio.astype(np.float32)
    if len(audio) == 0:
        return audio.astype(np.float32)
    duration = len(audio) / float(source_rate)
    target_len = max(1, int(round(duration * SR)))
    src_x = np.linspace(0.0, duration, num=len(audio), endpoint=False)
    dst_x = np.linspace(0.0, duration, num=target_len, endpoint=False)
    return np.interp(dst_x, src_x, audio).astype(np.float32)


def _decode_wav(path: Path):
    import numpy as np

    with wave.open(str(path), "rb") as wav:
        channels = wav.getnchannels()
        sample_width = wav.getsampwidth()
        rate = wav.getframerate()
        frames = wav.readframes(wav.getnframes())
    if sample_width != 2:
        raise ValueError(
            f"{path} uses {sample_width * 8}-bit WAV samples; only 16-bit PCM "
            "WAV is supported without ffmpeg")
    pcm = np.frombuffer(frames, dtype=np.int16)
    if channels > 1:
        pcm = pcm.reshape(-1, channels).mean(axis=1).astype(np.int16)
    audio = pcm.astype(np.float32) / 32768.0
    return _mono_float_to_int16(_resample_mono(audio, rate))


def _decode_with_ffmpeg(path: Path):
    import numpy as np
    from whisper_dictate.vp_transcribe import SR

    cmd = [
        "ffmpeg", "-v", "error", "-i", str(path),
        "-f", "s16le", "-acodec", "pcm_s16le", "-ac", "1", "-ar", str(SR), "-",
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, check=True)
    except FileNotFoundError as exc:
        raise RuntimeError(
            f"{path.suffix or 'audio'} files require ffmpeg unless they are "
            "16-bit PCM WAV. Install ffmpeg or pass a .wav file.") from exc
    except subprocess.CalledProcessError as exc:
        err = exc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"ffmpeg could not decode {path}: {err}") from exc
    pcm = np.frombuffer(proc.stdout, dtype=np.int16)
    return pcm.reshape(-1, 1)


def load_audio_file(path: str | Path):
    p = Path(path)
    if not p.exists():
        raise FileNotFoundError(p)
    if p.suffix.lower() == ".wav":
        try:
            return _decode_wav(p)
        except (wave.Error, ValueError):
            return _decode_with_ffmpeg(p)
    return _decode_with_ffmpeg(p)


def transcribe_file_event(
    model,
    path: str | Path,
    lang: str | None,
    *,
    model_name: str,
    stt_backend: str,
    device: str,
    compute_type: str,
) -> dict:
    from whisper_dictate.vp_transcribe import _transcribe_detail

    p = Path(path)
    pcm = load_audio_file(p)
    with contextlib.redirect_stdout(sys.stderr):
        result = _transcribe_detail(model, pcm, lang)
        post_result = postprocess_text(result.text)
    final_text = post_result.text
    return _base_event(
        event="file_transcription",
        text=final_text,
        dictionary_text=result.text,
        raw_text=result.raw_text or result.text,
        text_preview=_compact_text(final_text),
        text_chars=len(final_text),
        recording_s=result.duration_s,
        audio_duration_s=result.duration_s,
        post_boost_dbfs=result.post_boost_dbfs,
        compute_s=result.compute_s,
        real_time_factor=result.real_time_factor,
        language=result.language or lang or "auto",
        language_probability=result.language_probability,
        gate=result.gate,
        model=model_name,
        stt_backend=stt_backend,
        device=device,
        compute_type=compute_type,
        source_file=str(p),
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
    )


def print_transcribe_file_result(event: dict, *, as_json: bool) -> None:
    if as_json:
        print(json.dumps(event, ensure_ascii=False, separators=(",", ":")),
              flush=True)
    else:
        print(event.get("text", ""), flush=True)


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
    if history_enabled():
        _rust_json("append-history", event, "--path", str(history_path()))


def default_history_path() -> Path:
    if os.name == "nt":
        base = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        return Path(base) / "WhisperDictate" / "history.jsonl"
    return (
        Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state"))
        / "whisper-dictate"
        / "history.jsonl"
    )


def history_path() -> Path:
    raw = get_value("VOICEPI_HISTORY_JSONL")
    return Path(raw).expanduser() if raw else default_history_path()


def history_enabled() -> bool:
    return _truthy(get_value("VOICEPI_HISTORY_ENABLED", "1"))


def _history_event(event: dict) -> dict:
    keys = (
        "ts", "event", "text", "raw_text", "text_preview", "text_chars",
        "dictionary_text",
        "recording_s", "audio_duration_s", "compute_s", "real_time_factor",
        "language", "language_probability", "model", "stt_backend", "device",
        "compute_type", "inject_mode", "inject_strategy", "target_title",
        "target_process", "profile", "dictionary_replacements",
        "post_processor", "post_mode", "post_model", "post_latency_ms",
        "post_changed", "post_fallback", "post_error",
    )
    return {key: event[key] for key in keys if key in event}


def append_history(event: dict, path: Path | None = None) -> Path | None:
    if not history_enabled():
        return None
    p = path or history_path()
    _rust_json("append-history", event, "--path", str(p))
    return p


def read_history(limit: int = 20, path: Path | None = None) -> list[dict]:
    p = path or history_path()
    if not p.exists():
        return []
    rows: list[dict] = []
    with p.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(obj, dict):
                rows.append(obj)
    return rows[-max(0, limit):] if limit else rows


def last_history(path: Path | None = None) -> dict | None:
    rows = read_history(1, path)
    return rows[-1] if rows else None


def copy_last_to_clipboard(path: Path | None = None) -> str:
    item = last_history(path)
    if not item or not item.get("text"):
        raise RuntimeError("history is empty")
    import pyperclip

    text = str(item["text"])
    pyperclip.copy(text)
    return text


def reinject_last(path: Path | None = None) -> str:
    text = copy_last_to_clipboard(path)
    from pynput import keyboard

    kb = keyboard.Controller()
    with kb.pressed(keyboard.Key.ctrl):
        kb.press("v")
        kb.release("v")
    return text


def _run_rust_history_command(*args: str) -> bool:
    helper = _rust_helper()
    if not helper:
        return False
    try:
        r = subprocess.run(
            [helper, "history", *args],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=5,
        )
    except Exception as e:
        print(f"[history] {e}", file=sys.stderr, flush=True)
        return False
    if r.returncode != 0:
        print((r.stderr or r.stdout).strip(), file=sys.stderr, flush=True)
        return False
    if r.stdout:
        print(r.stdout.rstrip("\n"), flush=True)
    return True


def run_history_command(action: str, *, limit: int = 10, as_json: bool = False) -> None:
    try:
        if action == "list":
            if as_json:
                rows = read_history(limit)
                print(json.dumps(rows, ensure_ascii=False, sort_keys=True), flush=True)
            elif not _run_rust_history_command("list", str(limit)):
                for row in read_history(limit):
                    text = str(row.get("text", ""))
                    ts = row.get("ts", "")
                    backend = row.get("stt_backend", "")
                    print(f"{ts} [{backend}] {text}", flush=True)
        elif action == "last":
            if as_json:
                print(json.dumps(last_history() or {}, ensure_ascii=False, sort_keys=True), flush=True)
            elif not _run_rust_history_command("last"):
                print((last_history() or {}).get("text", ""), flush=True)
        elif action == "copy-last":
            text = copy_last_to_clipboard()
            print(f"copied: {text}", flush=True)
        elif action == "reinject-last":
            text = reinject_last()
            print(f"re-injected: {text}", flush=True)
        else:
            raise RuntimeError(f"unknown history action: {action}")
    except Exception as e:
        print(f"[history] {e}", file=sys.stderr, flush=True)
        raise


def _emit_worker_event(event: str, **fields) -> None:
    if not _truthy(os.environ.get("VOICEPI_WORKER_EVENTS")):
        return
    payload = {"event": event}
    payload.update({key: value for key, value in fields.items() if value is not None})
    print(
        "[worker-event] "
        + json.dumps(payload, ensure_ascii=True, sort_keys=True, separators=(",", ":")),
        file=sys.stderr,
        flush=True,
    )


def _sounddevice_input_info(sd) -> dict | None:
    try:
        default_device = getattr(getattr(sd, "default", None), "device", None)
        input_device = None
        if isinstance(default_device, (list, tuple)) and default_device:
            input_device = default_device[0]
        elif isinstance(default_device, int):
            input_device = default_device

        if input_device is None or input_device == -1:
            info = sd.query_devices(kind="input")
        else:
            info = sd.query_devices(input_device)
        if isinstance(info, dict):
            return info
    except Exception:
        return None
    return None


def _sounddevice_input_name(sd) -> str | None:
    info = _sounddevice_input_info(sd)
    if not info:
        return None
    name = str(info.get("name") or "").strip()
    return name or None


def _sounddevice_input_channels(sd) -> int:
    info = _sounddevice_input_info(sd)
    if not info:
        return 1
    try:
        channels = int(info.get("max_input_channels") or 1)
    except (TypeError, ValueError):
        return 1
    return max(1, channels)


def _sounddevice_capture_channel_candidates(max_channels: int) -> list[int]:
    max_channels = max(1, min(8, int(max_channels or 1)))
    candidates = [max_channels]
    for fallback in (2, 1):
        if fallback <= max_channels and fallback not in candidates:
            candidates.append(fallback)
    return candidates


def _audio_meter_level_from_dbfs(raw_dbfs: float) -> float:
    try:
        raw = float(raw_dbfs)
    except (TypeError, ValueError):
        return 0.0
    if raw != raw:
        return 0.0
    floor = -60.0
    ceiling = -12.0
    clamped = min(ceiling, max(floor, raw))
    normalized = (clamped - floor) / (ceiling - floor)
    return float(normalized ** 1.4)


def _select_active_channel_pcm(pcm):
    import numpy as np

    audio = np.asarray(pcm)
    if audio.ndim == 0:
        return audio.reshape(1, 1)
    if audio.ndim == 1:
        return audio.reshape(-1, 1)
    if audio.ndim > 2:
        audio = audio.reshape(audio.shape[0], -1)
    if audio.shape[1] <= 1:
        return audio.reshape(-1, 1)

    levels = audio.astype(np.float32)
    if getattr(audio, "dtype", None) is not None and audio.dtype.kind in ("i", "u"):
        levels = levels / 32768.0
    rms_by_channel = np.sqrt(np.mean(levels ** 2, axis=0))
    active_channel = int(np.argmax(rms_by_channel))
    return audio[:, active_channel:active_channel + 1]


def _audio_level_metrics(pcm) -> tuple[float, float, float]:
    import numpy as np

    mono = _select_active_channel_pcm(pcm)
    audio = mono.reshape(-1).astype(np.float32)
    if len(audio) == 0:
        return -120.0, 0.0, 0.0
    if getattr(mono, "dtype", None) is not None and mono.dtype.kind in ("i", "u"):
        audio = audio / 32768.0
    peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
    rms = float(np.sqrt(np.mean(audio ** 2)) or 1e-9)
    raw_dbfs = float(20 * np.log10(max(rms, 1e-9)))
    return raw_dbfs, peak, _audio_meter_level_from_dbfs(raw_dbfs)


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
        self._record_keydown_at = 0.0
        self._first_audio_at = 0.0
        self._first_audio_event = threading.Event()
        self._last_audio_level_event = 0.0
        self._stream = None
        self._arecord_proc = None
        self._capture_backend = ""
        self._audio_input_device = ""
        self._capture_channels = 1
        from pynput import keyboard
        self._kb = keyboard.Controller()
        self._inject_target_xwin: str | None = None   # XID captured at record start
        self._inject_target_title: str | None = None  # window title for debug log
        if self._effective_config.get("xkb_layout"):
            os.environ["VOICEPI_XKB_LAYOUT"] = self._effective_config["xkb_layout"]
        xkb = _detect_xkb_layout(lang) or ''
        self._xkb_layout = xkb
        if bool(os.environ.get('WAYLAND_DISPLAY')) and xkb:
            print(f"[inject] Rust keymap layout: {xkb}", flush=True)
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
            if bool(os.environ.get('WAYLAND_DISPLAY')) and xkb:
                print(f"[inject] Rust keymap layout: {xkb}", flush=True)

        from whisper_dictate import vp_audio
        from whisper_dictate import vp_postprocess
        from whisper_dictate import vp_transcribe

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
        vp_transcribe.VAD_SPEECH_PAD_MS = int(after.get("vad_speech_pad_ms", "200"))
        vp_transcribe.INITIAL_PROMPT = after.get("initial_prompt") or None
        vp_transcribe.STT_DEBUG = (after.get("stt_debug") or "").lower() not in (
            "", "0", "false", "no", "off")
        self.postprocess_settings = vp_postprocess.load_postprocess_settings()
        self.audio_ducker = register_active_ducker(AudioDucker.from_config())
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
            if not self._first_audio_event.is_set():
                self._first_audio_at = time.monotonic()
                self._record_started = self._first_audio_at
                self._first_audio_event.set()
            chunk = indata.copy()
            self.frames.append(chunk)
            self._emit_audio_level(chunk)

    def _arecord_reader(self, proc):
        # Read raw S16_LE mono 16kHz from arecord stdout into self.frames
        chunk = SR * 2 * 1  # 1 second of S16 mono = SR*2 bytes
        while self.recording:
            data = proc.stdout.read(chunk // 8)  # read ~125ms chunks
            if not data:
                break
            arr = np.frombuffer(data, dtype=np.int16).reshape(-1, 1)
            if not self._first_audio_event.is_set():
                self._first_audio_at = time.monotonic()
                self._record_started = self._first_audio_at
                self._first_audio_event.set()
            self.frames.append(arr)
            self._emit_audio_level(arr)

    def _emit_audio_level(self, pcm) -> None:
        now = time.monotonic()
        if now - self._last_audio_level_event < 0.12:
            return
        raw_dbfs, peak, level = _audio_level_metrics(pcm)
        self._last_audio_level_event = now
        _emit_worker_event(
            "audio",
            state="recording",
            level=round(level, 3),
            raw_dbfs=round(raw_dbfs, 1),
            peak=round(peak, 3),
            capture_backend=self._capture_backend,
            audio_device=self._audio_input_device,
            capture_channels=self._capture_channels,
        )

    def _start_arecord(self) -> tuple[str, str]:
        import subprocess
        self._capture_backend = "arecord"
        self._audio_input_device = f"arecord -D {_ARECORD_DEVICE}"
        self._capture_channels = 1
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
        return self._capture_backend, self._audio_input_device

    def _start_sounddevice(self) -> tuple[str, str]:
        import sounddevice as sd
        self._capture_backend = "sounddevice"
        self._audio_input_device = _sounddevice_input_name(sd) or "sounddevice default input"
        last_error = None
        for channels in _sounddevice_capture_channel_candidates(_sounddevice_input_channels(sd)):
            self._capture_channels = channels
            try:
                self._stream = sd.InputStream(
                    samplerate=SR,
                    channels=self._capture_channels,
                    dtype="int16",
                    callback=self._cb,
                )
                break
            except Exception as exc:
                last_error = exc
                self._stream = None
        if self._stream is None:
            raise last_error
        self._stream.start()
        return self._capture_backend, self._audio_input_device

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
        self._first_audio_event.clear()
        self._first_audio_at = 0.0
        self._last_audio_level_event = 0.0
        self.recording = True
        self._record_keydown_at = time.monotonic()
        self._record_started = 0.0
        self.audio_ducker.enter()
        _emit_worker_event("status", state="opening")
        if _ARECORD_DEVICE:
            self._capture_backend, self._audio_input_device = self._start_arecord()
        else:
            self._capture_backend, self._audio_input_device = self._start_sounddevice()
        first_audio_ready = self._first_audio_event.wait(timeout=0.2)
        startup_ms = int((time.monotonic() - self._record_keydown_at) * 1000)
        if not first_audio_ready:
            self._record_started = time.monotonic()
        _emit_worker_event(
            "status",
            state="recording",
            capture_backend=self._capture_backend,
            audio_device=self._audio_input_device,
            capture_channels=self._capture_channels,
            startup_ms=startup_ms,
            first_audio="ok" if first_audio_ready else "pending",
        )
        print(
            f"[cap] startup={startup_ms}ms first_audio="
            f"{'ok' if first_audio_ready else 'pending'} "
            f"capture_backend={self._capture_backend} "
            f"capture_channels={self._capture_channels}",
            flush=True,
        )
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
        _emit_worker_event(
            "status",
            state="transcribing",
            capture_backend=self._capture_backend,
            audio_device=self._audio_input_device,
            capture_channels=self._capture_channels,
        )
        try:
            if not self.frames:
                return
            pcm = np.concatenate(self.frames, axis=0).astype(np.int16)
            pcm = _select_active_channel_pcm(pcm).astype(np.int16)
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
        finally:
            _emit_worker_event(
                "status",
                state="ready",
                capture_backend=self._capture_backend,
                audio_device=self._audio_input_device,
                capture_channels=self._capture_channels,
            )

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
