"""The live push-to-talk dictation loop (``Dictate``).

Extracted from runtime.py — this is the core product path: hold the key,
capture audio (arecord or sounddevice), transcribe, post-process + format,
inject, and emit the utterance event. The audio-capture state machine is mixed
in from vp_capture.CaptureMixin; key detection from vp_keys.KeyBackendMixin;
text injection from vp_inject.InjectMixin.

numpy / sounddevice / faster_whisper stay lazy: importing this module must not
drag in the heavy ML/audio stack. The transcribe-side globals (``np``, ``SR``,
``_transcribe_detail`` …) are populated by ``_load_runtime_modules`` exactly as
they were in runtime.py; runtime.py delegates to it so existing callers keep
working.
"""
from __future__ import annotations

import os
import sys
import threading
import time

from whisper_dictate import vp_capture
from whisper_dictate.vp_audio_ducking import AudioDucker, register_active_ducker
from whisper_dictate.vp_capture import CaptureMixin, FIRST_AUDIO_WAIT_S, concat_capture_frames
from whisper_dictate.vp_config import (
    apply_config_to_environ, config_mtime, effective_config, load_config,
)
from whisper_dictate.vp_events import (
    _apply_profile_settings, _base_event, _compact_text,
    _emit_json, _emit_worker_event, _run_command_hook_and_annotate,
    _select_active_channel_pcm,
)
from whisper_dictate.vp_format import apply_format_commands
from whisper_dictate.vp_health import format_health_line
from whisper_dictate.vp_history import append_record_sinks
from whisper_dictate.vp_inject import InjectMixin
from whisper_dictate.vp_keymap import _detect_xkb_layout
from whisper_dictate.vp_keys import KeyBackendMixin
from whisper_dictate.vp_feedback import play_cue
from whisper_dictate.vp_postprocess import load_postprocess_settings, postprocess_text
from whisper_dictate.vp_preview import PreviewEngine, preview_enabled

# Populated lazily by _load_runtime_modules() (numpy + transcribe backend).
np = None
SR = 16000
STT_BACKEND = ""
_transcribe_detail = None
is_hallucination = None


def _load_runtime_modules() -> None:
    """Populate the lazy numpy + transcribe-backend globals used by Dictate.

    Mirrors runtime.py's loader; safe to call repeatedly. Kept here so the
    Dictate methods resolve ``np`` / ``_transcribe_detail`` / ``is_hallucination``
    / ``STT_BACKEND`` from this module's namespace (which is also what the unit
    tests patch). Also materialises the capture-side globals in vp_capture.
    """
    global np, SR, STT_BACKEND, _transcribe_detail, is_hallucination

    import numpy as np  # noqa: F811
    from whisper_dictate.vp_transcribe import (  # noqa: F811
        SR, STT_BACKEND, _transcribe_detail, is_hallucination,
    )
    vp_capture._load_runtime_modules()
    from whisper_dictate import vp_preview
    vp_preview._load_runtime_modules()


class Dictate(InjectMixin, KeyBackendMixin, CaptureMixin):
    def __init__(self, model: "WhisperModel", key: str, mode: str,
                 lang: str | None, *, json_output: bool = False,
                 metrics_jsonl: str | None = None, model_name: str = "",
                 device: str = "", compute_type: str = "",
                 model_load_s: float | None = None,
                 audio_source: str = "sounddevice"):
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
        # Source of audio frames for capture. "sounddevice" (default) opens
        # cpal/PortAudio via vp_capture; "rust-stdin" reads JSON-line events
        # piped in from the Rust controller (see vp_rust_audio_source). Only
        # set to "rust-stdin" when the supervisor was launched with
        # VOICEPI_AUDIO_BACKEND=rust AND the audio-in-rust feature is built in.
        self._audio_source = audio_source
        self._config_mtime = config_mtime()
        self._effective_config = effective_config()
        self.parakeet_min_seconds = float(
            self._effective_config.get("parakeet_min_seconds", "1.5"))
        self.min_record_seconds = float(
            self._effective_config.get("min_record_seconds", "0.5"))
        self.release_tail_ms = int(float(
            self._effective_config.get("release_tail_ms", "200")))
        # Live partial-transcription preview interval (seconds); "0" disables.
        # Read live at recording start (in _start), not cached here.
        self.preview_seconds = float(
            self._effective_config.get("preview_seconds", "3"))
        self._preview: PreviewEngine | None = None
        self.postprocess_settings = load_postprocess_settings()
        self.audio_ducker = register_active_ducker(AudioDucker.from_config())
        self.model_load_s = model_load_s
        self._restart_required_reported = False
        self._active_profile_name: str | None = None
        self.frames: list[np.ndarray] = []
        self.recording = False
        # Monotonic generation counter, bumped on every _start. A chord-cancel
        # is dispatched on a daemon thread that may be delayed past a release +
        # re-press; it captures the epoch at chord-detection time so a stale
        # cancel cannot discard the NEW recording (see _cancel_and_discard).
        self._record_epoch = 0
        # Set by the key backend when a chord forms on a bare-modifier PTT key
        # (e.g. Ctrl held, then C): the next stop discards audio, no transcribe.
        self._discard_recording = False
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
        # Actual capture sample rate. SR (16k) for arecord and 16k-native
        # sounddevice opens; set to the device default rate by _start_sounddevice
        # when a 16k open is rejected (the buffer is then resampled to SR at
        # consumption — see vp_capture._resample_capture_buffer).
        self._capture_rate = SR
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
        arecord_device = vp_capture._ensure_arecord_device()
        if arecord_device:
            print(f"[audio] using arecord -D {arecord_device} (PipeWire route)", flush=True)
        else:
            print("[audio] using sounddevice (direct ALSA)", flush=True)
        # Resolve the active input device early (no stream opened) so the field
        # is known before the first recording — keeps the UI's mic name shown
        # from `ready` rather than blank "Input pending". The truly-bound device
        # is re-derived when a recording opens the stream (_start_*).
        self._audio_input_device = vp_capture.resolve_startup_audio_device()
        # Iteration-3 review finding #1: when the Rust audio backend is in
        # play the supervisor is ALREADY producing pipeline events into
        # our stdin while we're still loading the model. Spawn the
        # long-lived reader NOW (not at first PTT press) so those frames
        # get drained as they arrive — without it, idle-time frames pile
        # up in the OS pipe and the very first press drains them as
        # stale audio (and a full pipe can block the bridge so stop()
        # hangs joining the writer). The reader drops frames when
        # ``recording`` is False, so this is safe to start immediately.
        if self._audio_source == "rust-stdin":
            from .vp_capture_rust_stdin import start_rust_stdin_capture
            self._cap_warned = False
            start_rust_stdin_capture(self)

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
        self._report_restart_required(after)
        self._apply_live_session_settings(after)
        self._apply_runtime_module_config(after)
        self.postprocess_settings = load_postprocess_settings()
        self.audio_ducker = register_active_ducker(AudioDucker.from_config())
        self._effective_config = after
        print("[config] reloaded live settings", flush=True)

    def _report_restart_required(self, after: dict[str, str]) -> None:
        """Warn once about changed settings that only take effect on restart."""
        restart_keys = {"stt_backend", "model", "parakeet_model", "device", "compute_type", "key"}
        changed_restart = [k for k in sorted(restart_keys) if self._effective_config.get(k) != after.get(k)]
        if changed_restart and not self._restart_required_reported:
            print(
                "[config] updated settings require restart/model reload: "
                + ", ".join(changed_restart),
                flush=True,
            )
            self._restart_required_reported = True

    def _apply_live_session_settings(self, after: dict[str, str]) -> None:
        """Apply settings that live on this Dictate instance (mode, lang, xkb)."""
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

    def _apply_runtime_module_config(self, after: dict[str, str]) -> None:
        """Push live-tunable settings into the vp_audio / vp_transcribe modules."""
        from whisper_dictate import vp_audio
        from whisper_dictate import vp_transcribe

        vp_audio.TARGET_DBFS = float(after.get("target_dbfs", "-20"))
        vp_audio.MIN_INPUT_DBFS = float(after.get("min_input_dbfs", "-55"))
        vp_audio.MIN_INPUT_SNR_DB = float(after.get("min_snr_db", "6"))

        vp_transcribe.BEAM_SIZE = int(after.get("beam_size", "1"))
        vp_transcribe.TEMPERATURES = vp_transcribe._parse_temperatures(after.get("temperature"))
        vp_transcribe.CONTEXT_MIN_SECONDS = float(after.get("context_min_seconds", "5"))
        self.parakeet_min_seconds = float(after.get("parakeet_min_seconds", "1.5"))
        self.min_record_seconds = float(after.get("min_record_seconds", "0.5"))
        vp_transcribe.MAX_CHARS_PER_SECOND = float(after.get("max_chars_per_second", "30"))
        self.release_tail_ms = int(float(after.get("release_tail_ms", "200")))
        self.preview_seconds = float(after.get("preview_seconds", "3"))
        vp_transcribe.VAD_THRESHOLD = float(after.get("vad_threshold", "0.3"))
        vp_transcribe.VAD_MIN_SILENCE_MS = int(after.get("vad_min_silence_ms", "600"))
        vp_transcribe.VAD_SPEECH_PAD_MS = int(after.get("vad_speech_pad_ms", "200"))
        # A --prompt override is authoritative for the whole run: don't let a live
        # config reload overwrite it with the saved initial_prompt value (#154).
        if not vp_transcribe.INITIAL_PROMPT_FORCED:
            vp_transcribe.INITIAL_PROMPT = after.get("initial_prompt") or None
        vp_transcribe.STT_DEBUG = (after.get("stt_debug") or "").lower() not in (
            "", "0", "false", "no", "off")

    def _reload_live_config_if_changed(self) -> None:
        mt = config_mtime()
        if mt <= self._config_mtime:
            return
        self._config_mtime = mt
        # Snapshot the audio-device setting BEFORE the reload so a live change to
        # it can drive an immediate optimistic status update (Fix 2 below).
        prev_audio_device = vp_capture._audio_device_setting()
        apply_config_to_environ()
        self._apply_effective_config(self._profiled_config(effective_config()))
        self._emit_audio_device_if_changed(prev_audio_device)

    def _emit_audio_device_if_changed(self, prev_setting: str) -> None:
        """Optimistically refresh the shown mic NAME on a live device change (Fix 2).

        The status display only learned the active ``audio_device`` when a
        recording opened the stream, so after the user picked a new mic and SAVED
        the UI kept showing the OLD name until the next recording. Here, when the
        live-reloaded ``VOICEPI_AUDIO_DEVICE`` actually CHANGED, we re-resolve the
        new device's NAME with the query-only resolver (``resolve_startup_audio_
        device`` — it opens NO stream) and emit a ``status=ready`` event carrying
        the new ``audio_device`` immediately, so the UI updates on save.

        Best-effort + never blocks/crashes: a name that can't be resolved degrades
        to "System default" inside the resolver, so we still emit *something*. The
        truly-bound device is re-derived when the next recording opens the stream
        (``_start_*``), so a wrong optimistic name self-corrects.

        Only fires when the worker is IDLE (not mid-recording/mid-transcription).
        The live-config reload also runs at the TOP of ``_stop_and_transcribe`` —
        where ``recording`` is still True and the ``transcribing`` status has not
        yet been emitted. Emitting ``state="ready"`` there would wrongly flip the
        UI out of its recording/processing state mid-pipeline if the user saved a
        new mic while holding the key. So gate on the idle flag and let the next
        recording's stream-open carry the new device name when not idle.
        """
        if getattr(self, "recording", False):
            return
        if vp_capture._audio_device_setting() == prev_setting:
            return
        try:
            audio_device = vp_capture.resolve_startup_audio_device()
        except Exception as exc:  # noqa: BLE001 — optimistic update is best-effort
            print(f"[cap] could not re-resolve audio device on reload (ignored): {exc}",
                  file=sys.stderr, flush=True)
            return
        self._audio_input_device = audio_device
        print(f"[cap] audio device changed live → {audio_device!r}", flush=True)
        _emit_worker_event(
            "status",
            state="ready",
            capture_backend=self._capture_backend,
            audio_device=audio_device,
            capture_channels=self._capture_channels,
        )

    def _should_skip_pcm(self, pcm: np.ndarray, recording_s: float) -> "str | None":
        """Return a skip-reason token if the clip should be discarded, else None.

        The return value is falsy (None) when the clip is acceptable, and a
        non-empty reason string when it should be skipped — so existing callers
        that just do ``if self._should_skip_pcm(...)`` continue to work.
        """
        # 0.3 s is the absolute misfire floor; a higher min_record_seconds raises
        # it (a user setting 0 still gets the 0.3 s protection via max()).
        min_seconds = max(0.3, getattr(self, "min_record_seconds", 0.5))
        if len(pcm) < SR * min_seconds:  # too short — almost certainly a misfire
            print("  (too short — hold the key while you speak)", flush=True)
            return "too_short"
        if self.stt_backend == "parakeet" and recording_s < self.parakeet_min_seconds:
            print(
                f"  (too short for Parakeet — speak at least {self.parakeet_min_seconds:.1f}s)",
                flush=True,
            )
            return "too_short"
        return None

    def _transcribe_pcm(self, pcm: np.ndarray) -> "tuple[object, str | None]":
        """Transcribe ``pcm`` and return ``(result, reason)`` where reason is a
        no_text token (``"too_quiet"``, ``"no_speech"``, ``"empty"``) when the
        result is unusable, or ``None`` on success.
        """
        try:
            result = _transcribe_detail(self.model, pcm, self.lang)
        except Exception as e:  # noqa: BLE001 — surface any failure
            print(f"  ✗ transcribe error: {e}", flush=True)
            return None, "no_speech"
        if not result.text:
            gate = getattr(result, "gate", "") or ""
            if "input too quiet" in gate:
                reason = "too_quiet"
            elif "no speech" in gate.lower():
                reason = "no_speech"
            else:
                reason = "empty"
            print("  (heard nothing — speak a touch louder / mic closer)", flush=True)
            return None, reason
        if is_hallucination(result.text):
            print(f"  (hallucination filtered: {result.text!r})", flush=True)
            return None, "no_speech"
        return result, None

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
        # Assembled from cohesive field groups so each part stays small and the
        # produced dict is identical to the old flat _base_event(...) call.
        return _base_event(
            event="utterance",
            text=final_text,
            dictionary_text=source_text,
            raw_text=result.raw_text or source_text,
            text_preview=_compact_text(final_text),
            text_chars=len(final_text),
            recording_s=recording_s,
            **self._transcription_event_fields(result),
            **self._inject_event_fields(inject_elapsed_ms),
            **self._post_event_fields(post_result),
            **self._format_event_fields(format_result),
        )

    def _transcription_event_fields(self, result) -> dict:
        """Audio-metric + model + transcription fields drawn from ``result``."""
        return {
            "audio_duration_s": result.duration_s,
            "post_boost_dbfs": result.post_boost_dbfs,
            "audio_raw_dbfs": result.raw_dbfs,
            "audio_peak": result.peak,
            "audio_gain": result.gain,
            "audio_noise_dbfs": result.noise_dbfs,
            "audio_snr_db": result.snr_db,
            "audio_input_status": result.input_status,
            "compute_s": result.compute_s,
            "real_time_factor": result.real_time_factor,
            "language": result.language or self.lang or "auto",
            "language_probability": result.language_probability,
            "gate": result.gate,
            "model": self.model_name,
            "stt_backend": self.stt_backend,
            "device": self.device,
            "compute_type": self.compute_type,
            "model_load_s": self.model_load_s,
            "segments": result.segments,
            "dictionary_terms": result.dictionary_terms,
            "dictionary_replacements": result.dictionary_replacements,
        }

    def _inject_event_fields(self, inject_elapsed_ms: int) -> dict:
        """Injection target / strategy / profile fields for the utterance event."""
        return {
            "inject_mode": self.mode,
            "inject_strategy": getattr(self, "_last_inject_strategy", None),
            "inject_elapsed_ms": inject_elapsed_ms,
            "target_title": getattr(self, "_inject_target_title", None),
            "target_process": getattr(self, "_inject_target_process", None),
            "profile": getattr(self, "_active_profile_name", None),
        }

    @staticmethod
    def _post_event_fields(post_result) -> dict:
        """Post-processing provenance fields drawn from ``post_result``."""
        return {
            "post_processor": post_result.provider,
            "post_mode": post_result.mode,
            "post_model": post_result.model,
            "post_latency_ms": post_result.latency_ms,
            "post_changed": post_result.changed,
            "post_fallback": post_result.fallback,
            "post_error": post_result.error or None,
            "post_redacted": post_result.redacted,
            "post_redactions": post_result.redactions or [],
        }

    @staticmethod
    def _format_event_fields(format_result) -> dict:
        """Format-command provenance fields drawn from ``format_result``."""
        return {
            "format_commands_enabled": format_result.enabled,
            "format_commands_set": format_result.command_set,
            "format_commands_changed": format_result.changed,
            "format_commands_applied": format_result.applied,
        }

    @staticmethod
    def _emit_health_line(metrics: dict) -> None:
        """Print the concise per-utterance ``[health]`` line at Basic+ verbosity.

        Gated on VOICEPI_DEBUG being truthy — both Basic (debug:on) and Verbose
        (debug:on,stt_debug:on) enable it, Off (debug:off) suppresses it. Read
        live from the env so a runtime Diagnostics-level change takes effect
        without restart (apply_config_to_environ runs on each live reload).
        """
        if (os.environ.get("VOICEPI_DEBUG") or "").strip().lower() in (
                "", "0", "false", "no", "off"):
            return
        print(format_health_line(metrics), flush=True)

    def _record_utterance_event(self, event: dict) -> None:
        _run_command_hook_and_annotate(event)
        if event.get("command_hook_error"):
            print(f"[hook] {event['command_hook_error']}", file=sys.stderr, flush=True)
        _emit_worker_event(
            "utterance",
            **{key: value for key, value in event.items() if key != "event"},
        )
        # Metrics JSONL is gated on JSON stdout: the metrics file is part of the
        # machine-readable integration surface, so it is only written when the
        # user has opted into structured output. A prefilled-but-unused path (the
        # UI suggests metrics.jsonl next to config.json) therefore stays inert
        # until "JSON stdout" is enabled.
        try:
            append_record_sinks(
                event,
                metrics_jsonl=self.metrics_jsonl,
                json_output=self.json_output,
            )
        except OSError as e:
            print(f"[sinks] could not write event sinks: {e}", file=sys.stderr, flush=True)
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
        # Bump the recording generation (getattr keeps object.__new__ test-bypass
        # instances, which skip __init__, working — like _discard_recording below).
        self._record_epoch = getattr(self, "_record_epoch", 0) + 1
        self._record_keydown_at = time.monotonic()
        self._record_started = 0.0
        self.audio_ducker.enter()
        _emit_worker_event("status", state="opening")
        try:
            if getattr(self, "_audio_source", "sounddevice") == "rust-stdin":
                # audio-in-rust: the Rust controller drives capture and pipes
                # frames into our stdin. The mixin spawns a reader thread that
                # accumulates frames into self.frames between speech_start /
                # speech_end events.
                self._capture_backend, self._audio_input_device = self._start_rust_stdin()
            elif vp_capture._arecord_device():
                self._capture_backend, self._audio_input_device = self._start_arecord()
            else:
                self._capture_backend, self._audio_input_device = self._start_sounddevice()
        except (Exception,) as exc:  # noqa: BLE001 — must never escape the PTT listener
            # CRITICAL: this runs on the pynput on_press listener thread (see
            # vp_keys.on_press → _start). An unguarded open/start failure here
            # (e.g. a mic that rejects a 16k WASAPI open, PaErrorCode -9997/-9999)
            # would propagate out of the listener callback and KILL THE WHOLE
            # WORKER. Swallow it, emit an actionable error event, and leave the
            # session idle + usable so the next PTT press can try again.
            self._handle_capture_start_failure(exc)
            return
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
        play_cue("start")
        self._start_preview()

    def _handle_capture_start_failure(self, exc: BaseException) -> None:
        """Recover from an open/start failure on the capture path (never raise).

        Tears the half-started recording down so the worker stays alive and the
        next PTT press can try again, then emits a single ``status=error`` event
        with an ACTIONABLE message naming the device and cause. Best-effort: any
        cleanup failure here is swallowed too — the whole point is that NOTHING
        escapes back to the pynput listener.
        """
        device = getattr(self, "_audio_input_device", "") or "the selected microphone"
        print(f"[cap] capture start failed on {device!r}: {exc}",
              file=sys.stderr, flush=True)
        self.recording = False
        try:
            self._stop_capture_streams()
        except Exception:  # noqa: BLE001 — cleanup must not re-raise
            pass
        try:
            self.audio_ducker.exit()
        except Exception:  # noqa: BLE001
            pass
        if isinstance(exc, vp_capture.DeviceUnusableError):
            # Honest "device unusable" (Fix 1): an EXPLICITLY-chosen mic that
            # would not open on ANY host API. Surface the actionable message
            # verbatim (it already names the device + the "pick another in
            # Settings" next step) and prefer its device label, so we never
            # claim a generic "format unsupported" or imply the wrong mic.
            device = getattr(exc, "device_label", None) or device
            message = exc.message
            reason = "device_unusable"
        else:
            # Generic capture-start failure: the cause may be an open failure, a
            # start failure, a format rejection, or an arecord/ALSA error on
            # Linux — don't assert "open"/"format unsupported" or name Windows
            # specifically, since any of those could be wrong for the actual path.
            message = (
                f"Could not start recording on {device} ({exc}). "
                "Try another microphone or check your system sound settings."
            )
            reason = "capture_open_failed"
        _emit_worker_event(
            "status",
            state="error",
            reason=reason,
            audio_device=device,
            error=message,
        )
        _emit_worker_event(
            "status",
            state="ready",
            capture_backend=self._capture_backend,
            audio_device=self._audio_input_device,
            capture_channels=self._capture_channels,
        )

    def _start_preview(self) -> None:
        """Begin the live partial-transcription preview for this recording.

        Only for the LOCAL whisper backend and a positive preview interval; the
        cloud ("openai") backend would spam a paid API and Parakeet is skipped
        for now. Read live (preview_seconds is refreshed by the live-config
        reload that runs at the top of _start). Any failure here is non-fatal —
        the preview is purely cosmetic.
        """
        if not preview_enabled(self.preview_seconds, self.stt_backend):
            return
        self._preview = PreviewEngine(self, self.preview_seconds)
        self._preview.start()
        print(f"[preview] live preview every {self.preview_seconds:g}s", flush=True)

    def _cancel_and_discard(self, epoch=None):
        """Abort the in-flight recording, discarding audio (chord on PTT key).

        Stops capture and drops the frames so nothing is transcribed or
        injected — mirrors a skip-reason exit but triggered by a foreign key
        joining a bare-modifier PTT key. Kept tiny; the key-state tracking lives
        in vp_keys. Safe to call when not recording (no-op).

        Runs on a daemon thread that may be delayed past a release + re-press.
        ``epoch`` is the recording generation captured by the key backend at
        chord-detection time: if a NEW recording has since started (epoch bumped
        in _start), this cancel is stale and must NOT discard the new clip, so we
        no-op unless it matches the current ``_record_epoch``.
        """
        if not self.recording:
            return
        if epoch is not None and epoch != getattr(self, "_record_epoch", epoch):
            return  # stale cancel for an already-finished recording
        self._discard_recording = True
        self._stop_and_transcribe()

    def _stop_and_transcribe(self):
        if not self.recording:
            return
        self._reload_live_config_if_changed()
        try:
            tail_s = max(0, self.release_tail_ms) / 1000.0
            # On a chord-cancel we discard the clip, so skip the release-tail
            # wait — there is nothing to capture. getattr keeps the object.__new__
            # test-bypass instances (which skip __init__) working.
            discard = getattr(self, "_discard_recording", False)
            if tail_s and not discard:
                time.sleep(tail_s)
            self.recording = False
            # Signal the live-preview thread to exit before the final pass so it
            # is not still holding TRANSCRIBE_LOCK. The final pass acquires the
            # lock blocking and MAY wait for at most one in-flight (bounded)
            # preview chunk to finish — acceptable.
            if self._preview is not None:
                self._preview.stop()
                self._preview = None
            self._stop_capture_streams()
            play_cue("stop")
        finally:
            self.audio_ducker.exit()
        if discard:
            # Chord formed on a bare-modifier PTT key: drop the audio entirely —
            # no transcription, no injection. Reset the flag for the next press.
            self._discard_recording = False
            self.frames = []
            _emit_worker_event("status", state="cancelled", reason="chord")
            print("[stt] dictation cancelled (chord)", flush=True)
            _emit_worker_event(
                "status",
                state="ready",
                capture_backend=self._capture_backend,
                audio_device=self._audio_input_device,
                capture_channels=self._capture_channels,
            )
            return
        _emit_worker_event(
            "status",
            state="transcribing",
            capture_backend=self._capture_backend,
            audio_device=self._audio_input_device,
            capture_channels=self._capture_channels,
        )
        try:
            if not self.frames:
                _emit_worker_event(
                    "status",
                    state="no_text",
                    reason="no_audio",
                )
                print("[stt] no text (no_audio)", flush=True)
                return
            pcm = concat_capture_frames(self.frames)
            if pcm is None:
                return
            pcm = _select_active_channel_pcm(pcm).astype(np.int16)
            # Resample native-rate capture (e.g. a 48k Yeti opened after a 16k
            # open was rejected) down to the model's 16k rate. No-op + bit-
            # identical when capture_rate == SR (16k-native devices). Done here,
            # at buffer consumption on the full mono buffer, not per-chunk — so
            # there are no chunk-boundary resample artifacts.
            capture_rate = getattr(self, "_capture_rate", SR)
            pcm = vp_capture._resample_capture_buffer(pcm, capture_rate)
            recording_s = self._recording_seconds(pcm)
            skip_reason = self._should_skip_pcm(pcm, recording_s)
            if skip_reason:
                _emit_worker_event(
                    "status",
                    state="no_text",
                    reason=skip_reason,
                    recording_s=round(recording_s, 2),
                )
                print(f"[stt] no text ({skip_reason}, {recording_s:.1f}s)", flush=True)
                return
            result, no_text_reason = self._transcribe_pcm(pcm)
            if result is None:
                _emit_worker_event(
                    "status",
                    state="no_text",
                    reason=no_text_reason,
                    recording_s=round(recording_s, 2),
                )
                # Transcription ran but produced nothing (hallucination gate /
                # empty / too-quiet) — a "we're off" signal worth a health line.
                # Pass the audio metrics available at this point so the health
                # line shows mic level, SNR and input status (was it too quiet?
                # too noisy?) instead of all-? placeholders.
                no_text_health: dict = {"no_text": True}
                try:
                    from whisper_dictate import vp_audio
                    # Mirror the transcribe path, which trims the dead trailing
                    # tail BEFORE computing capture metrics (vp_transcribe), so the
                    # no-text health line reports the SAME buffer the decode saw —
                    # otherwise raw_dbfs/snr_db disagree on long held-key tails
                    # (#260). The trim is scale-invariant (pure dB ratios + sample
                    # indices), so it is safe to run on the int16 pcm directly.
                    metrics_pcm, _trimmed_ms = vp_audio._trim_trailing_silence(
                        pcm.reshape(-1))
                    audio_metrics = vp_audio.compute_audio_metrics(metrics_pcm)
                    no_text_health["audio_raw_dbfs"] = audio_metrics.raw_dbfs
                    no_text_health["audio_snr_db"] = audio_metrics.snr_db
                    no_text_health["audio_input_status"] = audio_metrics.input_status
                    if audio_metrics.gain > 1.0:
                        no_text_health["audio_gain"] = audio_metrics.gain
                except Exception:  # noqa: BLE001 — metrics are best-effort
                    pass
                self._emit_health_line(no_text_health)
                print(f"[stt] no text ({no_text_reason}, {recording_s:.1f}s)", flush=True)
                return
            text = result.text
            # Surface a post-processing stage for the live pipeline card, but
            # only when a processor is actually going to run (not none/raw).
            if (
                self.postprocess_settings is not None
                and self.postprocess_settings.processor != "none"
                and self.postprocess_settings.mode != "raw"
            ):
                _emit_worker_event(
                    "status",
                    state="post-processing",
                    capture_backend=self._capture_backend,
                    audio_device=self._audio_input_device,
                    capture_channels=self._capture_channels,
                )
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
            self._emit_health_line(event)
            self._record_utterance_event(event)
        finally:
            _emit_worker_event(
                "status",
                state="ready",
                capture_backend=self._capture_backend,
                audio_device=self._audio_input_device,
                capture_channels=self._capture_channels,
            )
