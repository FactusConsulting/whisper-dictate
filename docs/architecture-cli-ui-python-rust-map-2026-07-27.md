# CLI + UI ↔ Python/Rust architecture map — 2026-07-27

Snapshot of which surfaces still call Python vs which run native Rust. Updated
after the Round 1–3 backend migration (feedback, history, metrics, ducking,
preview, target-profile), the native `devices test` in PR #600, and the ongoing
GUI-wiring phase (Doctor + List Devices buttons currently mid-migration).

## TL;DR

**No, the CLI does not exclusively use Rust.** Five verbs still shell to Python.
The Rust in-process dictation engine (`VOICEPI_DICTATE_ENGINE=rust`) IS
feature-complete relative to the Python engine after Round 1–3, but Python
remains the default engine — flipping the default is a separate step, gated on
GUI wiring + a full round of manual + smoke verification.

## CLI verbs (`whisper-dictate <verb>`)

| Verb | Native Rust | Shells to Python | Notes |
|---|:-:|:-:|---|
| `ui`, `settings` | ✅ | | Opens the egui Rust UI. UI itself may spawn Python worker for dictation. |
| `run` | | ✅ | Alias for the Python dictation loop (`runtime::run_terminal`). |
| `doctor` | ✅ | | Native (`doctor::handle_doctor`). |
| `bench` | | ✅ | Rust dispatcher shells to `runtime::benchmark_command()` → Python `--run-benchmark`. Two-step migration pending. |
| `corpus-record` | | ✅ | Rust dispatcher shells to `runtime::record_corpus_item_command()`. Two-step migration pending (same shape as retired `--test-audio-device`). |
| ~~`simulate-ptt`~~ | | | **Retired**; superseded by `simulate-session` (WAV + cloud) and `dictate-mic` (live + cloud). |
| `simulate-session` | ✅ | | Rust-engine WAV driver. Cloud STT only; hidden verb, CI/diag use. Replaces `simulate-ptt` for WAV-driven end-to-end checks. |
| `dictate-run` | ✅ | | Full in-process Rust dictation runtime (installs hotkey + coordinator + session sink). |
| `dictate-mic` | ✅ | | Feature-gated (`audio-capture`). Cpal capture → cloud STT → preview inject. |
| `install` | | ✅ | Python dep install. Legitimately Python. |
| `setup-ubuntu` | | ✅ | Wayland setup helper (Python). |
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
| Main dictation (start/stop worker) | opt-in | ✅ default | `VOICEPI_DICTATE_ENGINE=rust` runs full Rust in-process; default still spawns Python worker. |
| **Doctor** button | ❌ (migrating) | ✅ | Currently shells via `doctor_command()`. Being migrated to in-process Rust (PR in-flight). |
| **List audio devices** | ❌ (migrating) | ✅ | Currently shells via `audio_devices_command()`. Being migrated. |
| **Test audio device** button | ✅ | | Native since #600 (WASAPI on Windows, cpal on Linux). |
| List windows | | ✅ | Shells via `windows_command()`. |
| Install / Repair | | ✅ | Shells; legitimately Python for dep install. |
| Run benchmark | | ✅ | Shells via `benchmark_command()`. Requires the two-step migration. |
| Record corpus item | | ✅ | Shells via `record_corpus_item_command()`. Same shape. |
| Cancel task / Kill worker | ✅ | | Native (task management is Rust-side). |
| Debug log tail | ✅ | | Native. Reads runtime events channel. |
| History tab (list/reinject/copy-last) | ✅ | | Native. Same code path as the `history` CLI verb. |
| Dictionary tab | ✅ | | Native. |
| Config tab | ✅ | | Native. |

## Rust backend modules driving the in-process engine

All available on `VOICEPI_DICTATE_ENGINE=rust`. Each was added in Round 1–3
of the Python→Rust migration:

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

Modules whose PRIMARY caller is Python (not just a compat shim):

- `vp_dictate.py` + `runtime.py` — the Python dictation loop. Default engine.
- `vp_transcribe.py` — Python STT dispatcher. Called by Python engine.
- `vp_capture.py` + `vp_capture_rust_stdin.py` + `vp_rust_audio_source.py` —
  Python audio capture path (sounddevice + optional rust-stdin bridge).
- `vp_postprocess.py` — Python post-processing shim.
- `vp_inject.py` + `vp_inject_rust.py` — Python injection dispatch.
- `vp_setup.py` — Setup wizard (no Rust equivalent yet).
- `vp_benchmark.py` + `vp_corpus_record.py` — benchmark + corpus recording
  subsystems. Rust CLI verbs (`bench`, `corpus-record`) currently shell to these.
- `vp_history.py`, `vp_events.py`, `vp_health.py`, `vp_preview.py`,
  `vp_feedback.py`, `vp_audio_ducking.py`, `vp_dictionary_store.py` — Python
  primary implementations of features Rust ALSO has (parity ports). Used only
  by the Python engine path; dead when `VOICEPI_DICTATE_ENGINE=rust` is default.

## Migration status

Ranked by remaining Python-primary code that would go away once each item
lands:

1. **Flip default engine to Rust** — the biggest single retirement. All Round
   1–3 blockers are closed; the CLI + UI wiring phase is the current focus.
   When shipped, the ~5,900 lines of Python parity modules (vp_history,
   vp_events, vp_health, vp_preview, vp_feedback, vp_audio_ducking,
   vp_dictionary_store, plus most of vp_dictate/runtime/vp_capture/
   vp_transcribe/vp_inject/vp_postprocess) become dead and can be retired.

2. **GUI wiring — Doctor + List Devices** (in-flight): drops two Python
   subprocess call-sites from the UI.

3. **`corpus-record` two-step migration**: native cpal capture in Rust, then
   retire `vp_corpus_record.py` + `--record-corpus-item` argparse flag.

4. **`bench` two-step migration**: native Rust benchmark runner, then retire
   `vp_benchmark.py` (~505 lines) + report/paths helpers.

5. **Setup wizard**: no Rust equivalent yet. Needs a design pass.

6. **rust-stdin bridge retirement** (pending user go-ahead): drops ~450 lines
   of Python audio decoder once we accept that Rust-audio users switch to full
   Rust engine.

## Where this lives

- CLI dispatch: `src/rust/main.rs` (`match cli.command`).
- Rust engine entry: `src/rust/runtime/supervisor.rs::RuntimeSupervisor::start` →
  `runtime/in_process.rs::attempt_in_process_start` on
  `VOICEPI_DICTATE_ENGINE=rust`; falls back to spawning the Python worker
  otherwise.
- UI actions: `src/rust/ui/tasks.rs` (`run_background_command` gate),
  `src/rust/ui/corpus_record_tasks.rs` (corpus recording), `src/rust/ui/app.rs`
  (worker command construction).
- Python worker: `src/python/whisper_dictate/runtime.py` (argparse dispatch),
  `vp_dictate.py` (per-utterance loop).
