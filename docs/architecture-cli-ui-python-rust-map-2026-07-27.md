# CLI + UI ↔ Python/Rust architecture map — 2026-07-27

Snapshot of which surfaces still call Python vs which run native Rust. Updated
after the Round 1–3 backend migration (feedback, history, metrics, ducking,
preview, target-profile), the native `devices test` in PR #600, and the ongoing
GUI-wiring phase (Doctor + List Devices buttons currently mid-migration).

## TL;DR

**The default dictation engine is now native Rust (Phase 1 flip).** The
in-process Rust runtime — hotkey listener + coordinator + session sink — runs
directly in the UI process; no Python worker is spawned for dictation on a
default install. `VOICEPI_DICTATE_ENGINE=python` is a temporary safety-valve
opt-out kept for one release cycle so operators can fall back if the Rust
engine misbehaves; the Python engine modules (vp_dictate.py, vp_history,
vp_events, vp_feedback, vp_audio_ducking, vp_preview, vp_dictionary_store,
vp_health) get retired in the Phase 2 PR after user smoke on Ubuntu Wayland.
Five non-dictation verbs still shell to Python (install, setup-ubuntu, run
shims listed below).

## CLI verbs (`whisper-dictate <verb>`)

| Verb | Native Rust | Shells to Python | Notes |
|---|:-:|:-:|---|
| `ui`, `settings` | ✅ | | Opens the egui Rust UI. UI itself may spawn Python worker for dictation. |
| `run` | ✅ default | opt-out only | Default is the native Rust in-process engine (Phase 1 flip). `VOICEPI_DICTATE_ENGINE=python` falls back to the Python `runtime::run_terminal` loop as a transition-window safety valve; retired in Phase 2. |
| `transcribe-file` | ✅ | | Public one-shot 16 kHz mono WAV transcription through configured cloud STT, or local whisper.cpp when built with `whisper-rs-local` (shipping releases include it; default/lightweight source builds do not). Text or JSON output; no Python fallback. Available in Rust-controller distributions; the current legacy Nix derivation does not expose it. |
| `doctor` | ✅ | | Native (`doctor::handle_doctor`). |
| `bench` | ✅ | | Native Rust runner (`benchmark::native`) as of step 2 of the retirement (#626); stock dev builds without `whisper-rs-local`+`audio-capture` return a rebuild hint. |
| `corpus-record` | ✅ | | Native cpal recorder (`corpus_record_native::run_native`) as of step 2 of the retirement (#629); stock dev builds without `audio-capture` return a rebuild hint. |
| ~~`simulate-ptt`~~ | | | **Retired (#627)**; superseded by `simulate-session` (WAV + cloud) and `dictate-mic` (live + cloud). |
| `simulate-session` | ✅ | | Rust-engine WAV driver. Cloud STT only; hidden verb, CI/diag use. Replaces `simulate-ptt` for WAV-driven end-to-end checks. |
| `dictate-run` | ✅ | | Full in-process Rust dictation runtime (installs hotkey + coordinator + session sink). |
| `dictate-mic` | ✅ | | Feature-gated (`audio-capture`). Cpal capture → cloud STT → preview inject. |
| `install` | | ✅ | Python dep install. Legitimately Python. |
| `setup-ubuntu` | | ✅ | Wayland setup helper (Python). |
| `setup`, `export-config` | ✅ | | Native schema-driven headless setup and effective-config export; secrets are redacted unless explicitly included. |
| `model-capacity` | ✅ | | Native VRAM query. |
| `config` (get/set/path/list) | ✅ | | Native. |
| `dictionary` (add/prompt/list) | ✅ | | Native. Live-reloading. |
| `history` (list/last/copy-last/reinject/search) | ✅ | | Native. Reads the JSONL Rust session writes (as of #605). |
| `hotkey` (capture) | ✅ | | Native. `--driver evdev` for Wayland; `--chord` override (#611). |
| `self-test` (all sub-verbs) | ✅ | | 10 total: ptt-wedge, injection-idempotency, audio-capture, whisper-load, plus feedback, audio-ducking, profile-match, history-write, metrics-write, preview (#621). |
| `inject-text` | ✅ | | Native. Enigo backend + wayland helper chain. |
| `devices` (list/test) | ✅ | | Native cpal probe since #600. |
| `models` (catalog/download/verify) | ✅ | | Native. |
| `format-text` | ✅ | | Native. |
| Hidden dispatch helpers (`dictate-ops`, `dictionary-runtime`, `append-jsonl`, `apply-profile`, `command-hook`, `redact-text`, `postprocess`, `external-api`, `health`, `worker-event`, `inject`, `privacy`, `transcribe-wav`, `transcribe-server`) | ✅ | | All native — used by Python worker to shell BACK to Rust helpers. |

## UI actions (egui Rust)

| Action | In-process Rust | Shells to Python | Notes |
|---|:-:|:-:|---|
| Main dictation (start/stop worker) | ✅ default | opt-out only | Default is the full Rust in-process runtime (Phase 1 flip). `VOICEPI_DICTATE_ENGINE=python` spawns the Python worker as a transition-window safety valve; retired in Phase 2. |
| **Doctor** button | ❌ (migrating) | ✅ | Currently shells via `doctor_command()`. Being migrated to in-process Rust (PR in-flight). |
| **List audio devices** | ❌ (migrating) | ✅ | Currently shells via `audio_devices_command()`. Being migrated. |
| **Test audio device** button | ✅ | | Native since #600 (WASAPI on Windows, cpal on Linux). |
| List windows | | ✅ | Shells via `windows_command()`. |
| Install / Repair | | ✅ | Shells; legitimately Python for dep install. |
| Run benchmark | ✅ | | Native as of #626 — `run_benchmark` calls `benchmark::native::run_to_writer` on a background thread and captures output for the existing results parser. |
| Record corpus item | ✅ | | Native on `audio-capture` builds (#629) — background thread calls `corpus_record_native::run_native_to_string` and hands JSON events to the same UI parser. Dev builds without `audio-capture` log a "feature not enabled" message. |
| Cancel task / Kill worker | ✅ | | Native (task management is Rust-side). |
| Debug log tail | ✅ | | Native. Reads runtime events channel. |
| History tab (list/reinject/copy-last) | ✅ | | Native. Same code path as the `history` CLI verb. |
| Dictionary tab | ✅ | | Native. |
| Config tab | ✅ | | Native. |

## Rust backend modules driving the in-process engine

All ship on the default engine after the Phase 1 flip (unset env → Rust;
`VOICEPI_DICTATE_ENGINE=rust` is the explicit equivalent). Each was added
in Round 1–3 of the Python→Rust migration:

| Module | Rust path | Status | CLI test verb |
|---|---|---|---|
| History JSONL sink | `dictate::session::history_sink` | Wired in `make_real_session` | `self-test history-write` |
| Feedback cues (start/stop sound) | `dictate::feedback` | Wired via `with_cue_sink` | `self-test feedback` |
| Metrics JSONL sink | `dictate::session::metrics_sink` | Wired via `with_optional_metrics_sink` | `self-test metrics-write` |
| Audio ducking (WASAPI) | `dictate::audio_ducking` | Wired via `with_ducker` | `self-test audio-ducking` |
| Live preview (interim transcript) | `dictate::session::preview` | Wired via `with_optional_preview_engine` on the local Whisper backend | `self-test preview` |
| Target-profile matching | `dictate::profile` + `platform::foreground_window` | Wired via `with_profile_matcher` (12 override keys honored) | `self-test profile-match` |
| Audio capture (cpal + rubato + VAD) | `audio/**` (`audio-in-rust` feature) | Native | `self-test audio-capture` |
| Hotkey (rdev + evdev drivers) | `hotkey/**` (`rust-hotkeys` feature) | Native | `hotkey capture` |
| Text injection (enigo + wayland helpers) | `injection/**` (`rust-injection` feature) | Native | `inject-text` |
| Local Whisper transcription | `whisper::local` (`whisper-rs-local` feature) | Native | `self-test whisper-load` |
| Cloud STT (Groq / OpenAI) | `dictate::backends::cloud_transcribe` | Native | `simulate-session` |
| Post-processing (LLM chat) | `postprocess/**` | Native (default since #566) | (no dedicated verb) |
| Format commands | `formatting` | Native (#525) | `format-text` |
| Dictionary | `dictionary/**` | Native, live-reloading | `dictionary prompt` |
| Doctor | `doctor` | Native | `doctor` |
| Devices probe | `audio::device_probe` (`audio-capture` feature) | Native (#600) | `devices test` |

## Python modules still primary

Modules whose PRIMARY caller is Python (not just a compat shim). After the
Phase 1 default flip these only run when the operator explicitly sets
`VOICEPI_DICTATE_ENGINE=python`; they get physically deleted in the
Phase 2 PR once the Ubuntu Wayland smoke confirms the Rust default is
stable:

- `vp_dictate.py` + `runtime.py` — the Python dictation loop. Now the
  safety-valve fallback engine, no longer the default.
- `vp_transcribe.py` — Python STT dispatcher. Called by Python engine.
- `vp_capture.py` + `vp_capture_rust_stdin.py` + `vp_rust_audio_source.py` —
  Python audio capture path (sounddevice + optional rust-stdin bridge).
- `vp_postprocess.py` — Python post-processing shim.
- `vp_inject.py` + `vp_inject_rust.py` — Python injection dispatch.
- `vp_setup.py` — Setup wizard (no Rust equivalent yet).
- `vp_benchmark_paths.py` — corpus-loader helpers surviving after `vp_benchmark.py`
  retirement (#626); may be dropped when the last Python consumer goes.
- `vp_history.py`, `vp_events.py`, `vp_health.py`, `vp_preview.py`,
  `vp_feedback.py`, `vp_audio_ducking.py`, `vp_dictionary_store.py` — Python
  primary implementations of features Rust ALSO has (parity ports). Only
  used on the `VOICEPI_DICTATE_ENGINE=python` opt-out path; retired in the
  Phase 2 PR.

## Migration status

Ranked by remaining Python-primary code that would go away once each item
lands:

1. **Phase 2: physically delete the Python engine modules** — the follow-up
   PR after the Phase 1 default flip (this doc). Deletes the ~5,900 lines of
   Python parity modules (vp_history, vp_events, vp_health, vp_preview,
   vp_feedback, vp_audio_ducking, vp_dictionary_store, plus most of
   vp_dictate/runtime/vp_capture/vp_transcribe/vp_inject/vp_postprocess) and
   the `VOICEPI_DICTATE_ENGINE=python` safety-valve opt-out. Gated on user
   confirmation the Ubuntu Wayland smoke passed on Phase 1.

2. **Setup wizard**: no Rust equivalent yet. Needs a design pass.

3. **rust-stdin bridge retirement** (pending user go-ahead): drops ~450 lines
   of Python audio decoder once we accept that Rust-audio users switch to full
   Rust engine.

**Recently landed** (already reflected in the tables above):

- GUI Doctor + List Devices → in-process Rust (#623)
- `bench` two-step: native Rust + Python retirement (#625, #626)
- `corpus-record` two-step: native Rust + Python retirement (#624, #629)
- `simulate-ptt` retirement (#627): superseded by `simulate-session` + `dictate-mic`

## Where this lives

- CLI dispatch: `src/rust/main.rs` (`match cli.command`).
- Rust engine entry: `src/rust/runtime/supervisor.rs::RuntimeSupervisor::start` →
  `runtime/in_process.rs::attempt_in_process_start` on the default (unset env
  or `VOICEPI_DICTATE_ENGINE=rust`); falls back to spawning the Python worker
  on `VOICEPI_DICTATE_ENGINE=python` or on a Rust-engine startup failure
  (features missing, config load failed, etc.).
- UI actions: `src/rust/ui/tasks.rs` (`run_background_command` gate),
  `src/rust/ui/corpus_record_tasks.rs` (corpus recording), `src/rust/ui/app.rs`
  (worker command construction).
- Python worker: `src/python/whisper_dictate/runtime.py` (argparse dispatch),
  `vp_dictate.py` (per-utterance loop).
