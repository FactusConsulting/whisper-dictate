"""The live push-to-talk dictation loop (``Dictate``).

Extracted from runtime.py — this is the core product path: hold the key,
capture audio (arecord or sounddevice), transcribe, post-process + format,
inject, and emit the utterance event. Key detection is mixed in from
vp_keys.KeyBackendMixin; text injection from vp_inject.InjectMixin.

numpy / sounddevice / faster_whisper stay lazy: importing this module must not
drag in the heavy ML/audio stack. The transcribe-side globals (``np``, ``SR``,
``_transcribe_detail`` …) are populated by ``_load_runtime_modules`` exactly as
they were in runtime.py; runtime.py delegates to it so existing callers keep
working.
"""
from __future__ import annotations

import os
import subprocess
import sys
import threading
import time

from whisper_dictate.vp_audio_ducking import AudioDucker, register_active_ducker
from whisper_dictate.vp_config import (
    apply_config_to_environ, config_mtime, effective_config, load_config,
)
from whisper_dictate.vp_events import (
    _apply_profile_settings, _audio_level_metrics, _base_event, _compact_text,
    _emit_json, _emit_worker_event, _run_command_hook_and_annotate,
    _select_active_channel_pcm, _sounddevice_capture_channel_candidates,
    _sounddevice_input_channels, _sounddevice_input_name, _sounddevice_stream_kwargs,
)
from whisper_dictate.vp_format import apply_format_commands
from whisper_dictate.vp_history import _append_history, _append_jsonl
from whisper_dictate.vp_inject import InjectMixin
from whisper_dictate.vp_keymap import _detect_xkb_layout
from whisper_dictate.vp_keys import KeyBackendMixin
from whisper_dictate.vp_postprocess import load_postprocess_settings, postprocess_text

_ARECORD_DEVICE: str | None = None  # set once at startup
FIRST_AUDIO_WAIT_S = 0.35

# Populated lazily by _load_runtime_modules() (numpy + transcribe backend).
np = None
SR = 16000
STT_BACKEND = ""
_transcribe_detail = None
is_hallucination = None
_find_arecord_device = None


def _load_runtime_modules() -> None:
    """Populate the lazy numpy + transcribe-backend globals used by Dictate.

    Mirrors runtime.py's loader; safe to call repeatedly. Kept here so the
    Dictate methods resolve ``np`` / ``_transcribe_detail`` / ``is_hallucination``
    / ``STT_BACKEND`` / ``_find_arecord_device`` from this module's namespace
    (which is also what the unit tests patch).
    """
    global np, SR, STT_BACKEND, _transcribe_detail, is_hallucination, _find_arecord_device

    import numpy as np  # noqa: F811
    from whisper_dictate.vp_audio import _find_arecord_device  # noqa: F811
    from whisper_dictate.vp_transcribe import (  # noqa: F811
        SR, STT_BACKEND, _transcribe_detail, is_hallucination,
    )


class Dictate(InjectMixin, KeyBackendMixin):
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
        vp_transcribe.CONTEXT_MIN_SECONDS = float(after.get("context_min_seconds", "5"))
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
            for kwargs in _sounddevice_stream_kwargs(self._capture_channels, self._cb):
                try:
                    self._stream = sd.InputStream(**kwargs)
                    break
                except Exception as exc:
                    last_error = exc
                    self._stream = None
            if self._stream is not None:
                break
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
            audio_raw_dbfs=result.raw_dbfs,
            audio_peak=result.peak,
            audio_gain=result.gain,
            audio_noise_dbfs=result.noise_dbfs,
            audio_snr_db=result.snr_db,
            audio_input_status=result.input_status,
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
        _emit_worker_event(
            "utterance",
            **{key: value for key, value in event.items() if key != "event"},
        )
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
        first_audio_ready = self._first_audio_event.wait(timeout=FIRST_AUDIO_WAIT_S)
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
