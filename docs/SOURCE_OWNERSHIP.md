# Source Ownership: Rust and Python

This repo is intentionally mixed Rust/Python. The rule of thumb is:

- Rust owns deterministic controller logic, UI, config/schema, installers,
  process supervision, JSON helpers, platform-safe output and features that
  should run before Python ML dependencies load.
- Python owns the live dictation worker where the core dependencies are still
  Python libraries: `faster-whisper`, NumPy, sounddevice, pynput, evdev and
  audio/file decoding glue. (The NeMo/Parakeet adapter was dropped in Wave 8
  of #348.)

## Rust

Rust source lives in `src/rust`.

| Area | Files | Responsibility |
|---|---|---|
| CLI/controller | `cli.rs`, `main.rs` | Public `whisper-dictate` command, hidden worker helpers, command dispatch. |
| Desktop UI | `ui.rs`, `ui/*` | egui settings/runtime UI, API key UI, tabs, platform controls. |
| Runtime supervision | `runtime.rs`, `runtime/*_tests.rs` | Starts/stops/restarts the Python worker, exports effective config as `VOICEPI_*`, runs install/doctor/setup commands, parses worker events, keeps Windows launch behavior unified. |
| Config schema | `config.rs` | Config path, defaults, validation, save/load, restart-key checks, runtime env export. |
| Dictionary | `dictionary.rs` | JSON/text dictionary parsing, prompt term caps, deterministic replacements, CLI add/status/open/replace, runtime dictionary helper. |
| Injection core | `injection.rs` | Rust text injection helper and Wayland keycode maps/layout handling. |
| Text formatting | `formatting.rs` | Deterministic spoken punctuation/line-break command replacement. |
| Redaction/privacy | `redaction.rs`, `privacy.rs` | Cloud-safe redaction helper and local-only backend/processor guard. |
| Profiles | `profiles.rs` | Target-profile matching and settings overlay. |
| Telemetry/history | `telemetry.rs` | JSONL append, worker events, history read/list/last. |
| Command hooks | `command_hook.rs` | Safe command-hook argv parsing, stdin JSON handoff, timeout/error reporting. |
| Cloud helper | `cloud_api.rs` | OpenAI-compatible audio transcription helper used by the Python STT adapter when available. |
| Model capacity | `model_capacity.rs` | NVIDIA VRAM parsing and model-fit guidance without loading Python. |
| Packaging metadata | `Cargo.toml`, `Cargo.lock`, build scripts under `src/rust` | Rust workspace/crate dependencies and Windows resource embedding. |

Rust should be the default home for new non-ML logic. Add regression tests in
Rust when moving a Python behavior here.

## Python

Python source lives in `src/python/whisper_dictate`.

| Area | Files | Why it remains Python |
|---|---|---|
| Worker orchestration | `runtime.py` | Live push-to-talk loop, hotkey lifecycle, audio capture start/stop, model loading, utterance lifecycle and legacy terminal command dispatch still depend on Python runtime libraries. |
| Audio DSP/capture helpers | `vp_audio.py` | NumPy-based dBFS/SNR/gating and Linux audio-device probing are shared by live STT and calibration. |
| STT adapters | `vp_transcribe.py`, `vp_external_api.py` | `faster-whisper` and OpenAI-compatible fallback are Python library boundaries. Rust owns privacy/dictionary/cloud helper paths around them where practical. (The `vp_parakeet.py` NeMo adapter was deleted in Wave 8 of #348.) |
| Injection orchestration | `vp_inject.py` | Target-window detection, clipboard/pynput/ydotool fallback orchestration and focus restore still sit in the Python worker. Rust owns the keymap/helper path where parity exists. |
| CLI compatibility | `vp_cli.py` | Argparse surface for `whisper-dictate run -- ...`, debug setting dump and Python-only direct execution compatibility. Public top-level subcommands should prefer Rust. |
| Config compatibility/live reload | `vp_config.py` | Temporary Python compatibility layer for direct Python execution and live reload inside the worker. Normal Rust launches now export effective config into the worker environment before imports. |
| Post-processing orchestration | `vp_postprocess.py` | Loads post-processing settings, talks to Ollama/OpenAI-compatible chat fallback and restores local redactions. Rust owns redaction and local-only checks. |
| Benchmark/evaluation | `vp_benchmark.py` | Corpus loading, WER/CER annotation and multi-backend benchmark orchestration around Python STT models. |

## Migration Guidance

Move behavior to Rust when it is deterministic controller logic, structured
parsing, schema/default handling, installer/runtime behavior, Windows launch
behavior, JSONL storage, platform guards or UI-backed settings.

Keep behavior in Python when the value is mostly in Python ML/audio/input
libraries or when moving it would mean reimplementing unstable platform glue
without a clear user-facing win.

Good Rust candidates:

- `vp_config.py` live reload/effective-config helpers, once the worker can ask
  Rust for reload state without preserving duplicate schema in Python.
- More post-processing settings/prompt construction and cloud chat transport,
  if we want one Rust OpenAI-compatible HTTP path for both STT and cleanup.

Keep in Python until there is a clear replacement:

- `faster-whisper` and model-specific audio transcription glue. (The Parakeet
  / NeMo backend was removed in Wave 8 of #348 and is not coming back unless
  a future wave finds a Rust-friendly NeMo path.)
- NumPy/sounddevice/arecord capture and calibration paths.
- pynput/evdev hotkey loops and target-window detection, especially on Windows
  where behavior must stay boring and predictable.

When moving behavior from Python to Rust, delete stale Python tests rather than
renaming them as placeholders. Add Rust unit/integration tests for the moved
behavior and keep Python boundary tests only where the Python worker still calls
into Rust.
