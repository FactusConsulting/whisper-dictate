<!--
Written by an audit agent 2026-07-16. Reflects v1.21.0 baseline immediately
after the reset-to-v1.19.0-rc.3 recovery. Guides the incremental Rust
reintroduction effort — see the prioritized refactor list.
-->

# Library-first architecture audit — v1.21.0 baseline

Date: 2026-07-16
Branch: main @ v1.21.0 (restored v1.19.0-rc.3 Python-worker baseline)
Author: architecture-audit subagent
Scope: 11 user-facing features. Zero code changes — analysis only.

## Summary

- **Total features assessed:** 11
- **Library-first + CLI + headless-testable:** 4 (config, dictionary, formatting, model management)
- **Partial:** 5 (transcription, injection, hotkey/PTT, history, prompt building)
- **Needs work:** 2 (audio devices, runtime orchestration)
- **UI-only by design (out of scope for library-first):** 1 (egui rendering — has good testable helpers)

The codebase is midway through a multi-wave "Python-removal" refactor (issue #348).
Waves 4-A/B/C, 5, 6, 7-B/C, and 8 have already moved substantial chunks of pure
logic into Rust with a hidden `whisper-dictate <subcommand>` JSON-RPC surface the
Python worker shells out to. Roughly two-thirds of user-facing logic already has
a Rust library home *and* a CLI/JSON-RPC adapter. The unfinished third is
dominated by two hotspots:

1. **The live PTT loop** (`vp_dictate.py` + `runtime.py` + `vp_keys*.py`) — the
   Rust `dictate::session::DictateSession` state machine is complete but no
   production path calls it yet; Python still owns the hot orchestration loop.
2. **Audio capture + STT model loading** — cpal + rubato + Silero exist behind
   `--features audio-in-rust` and the Rust `whisper::dispatch` server exists
   behind `--features whisper-rs-local`, but the default binaries still shell
   out to Python for both. `faster-whisper` is the shipping local backend.

The prioritised refactor list at the end targets the highest value-per-effort
gaps under those two hotspots.

Layout target (repeated for reference):
```
Core module (lib) ← tests hit here
    ↑             ↑
UI (egui)      CLI (clap)
```

---

## Per-feature assessment

### 1. Transcription (audio → text)

- **Core logic (Python):** `src/python/whisper_dictate/vp_transcribe.py`
  (~1406 LOC) — `faster-whisper` in-process, cloud STT via
  `vp_external_api.py`, hallucination filter, decode temperatures, VAD gates.
  Called from `runtime.py::_load_model` / `_run_session` (line 825).
- **Core logic (Rust — parallel/prep):**
  - `src/rust/whisper/mod.rs`, `src/rust/whisper/dispatch.rs` (342 LOC),
    `src/rust/whisper/local/mod.rs`, `src/rust/whisper/idle/` — whisper.cpp
    integration, in-process worker, idle unloading. Gated on
    `--features whisper-rs-local`.
  - `src/rust/whisper/protocol.rs` (JSON envelopes) and `src/rust/whisper/wav.rs`
    (decode helpers) compile unconditionally.
  - `src/rust/cloud_api/transcribe.rs` — OpenAI-compatible audio transcription
    (already a callable helper the Python worker uses when available; see
    `docs/SOURCE_OWNERSHIP.md:30`).
  - `src/rust/dictate/backend.rs` — `BackendKind` (Whisper vs OpenAI)
    validator + label; unit-tested; opt-in shell-out via
    `VOICEPI_DICTATE_BACKEND=rust` (`cli.rs:81-86`).
- **CLI adapter:**
  - Public: `whisper-dictate cloud-transcribe --base-url ... --api-key ...
    --model ... --audio-wav-path ...` (cli.rs:122-145) — cloud path is
    callable stand-alone.
  - Hidden: `transcribe-wav [--probe]` (cli.rs:207-217) and
    `transcribe-server` (cli.rs:229-230) — local whisper.cpp path,
    stdin JSON envelope, only functional with `--features whisper-rs-local`.
  - Python: `whisper-dictate run --transcribe-file PATH` (vp_cli.py:228-231) —
    full worker path around a file.
- **Headless testable:** **Partial.**
  - Rust protocol/wav/dispatch/local: unit tests present (`whisper/*_tests.rs`,
    `wav_tests.rs`); the whisper.cpp integration only runs on feature-enabled
    CI legs.
  - Python: `test_transcribe_file.py`, `test_rust_transcribe.py`,
    `test_rust_transcribe_server.py`, `test_stt.py`, `test_groq_integration.py`.
  - No real `faster-whisper` unit tests exist; the model is treated as an
    injected object with stub replacements.
- **Gap:**
  - The Rust `LocalWhisper` + `IdleUnloadingModel` are ready but the
    production path (`vp_transcribe._transcribe_detail`) still calls
    `faster-whisper` directly; the `transcribe-server` subcommand is only used
    when the user opts in via `VOICEPI_TRANSCRIBE_BACKEND=rust`.
  - No stand-alone `transcribe-file PATH` public CLI in Rust — you have to
    shell in through the full Python worker to transcribe a single file
    outside a dictation session.
- **Effort estimate:** **large.** Making Rust the default requires shipping
  `--features whisper-rs-local` by default (CMake/whisper.cpp toolchain on
  the release matrix), migrating decode-temperature/hallucination-filter
  parity to Rust (Python `vp_transcribe.py:1042-1230`), and cutting Python
  over to shelling out to `transcribe-server`.

---

### 2. Text injection (worker types the transcript into the active window)

- **Core logic (Python):** `src/python/whisper_dictate/vp_inject.py`
  (~631 LOC) — Wayland/ydotool, X11/pynput, paste/type modes, clipboard save
  and restore, target-window/process detection. Mixin on `Dictate`.
- **Core logic (Rust):**
  - `src/rust/injection/mod.rs` + submodules `dispatcher.rs`,
    `enigo_backend.rs`, `fallback.rs`, `keymap.rs`, `linux_helpers.rs`,
    `paste.rs`, `wayland.rs`. Cross-platform via `enigo` behind the
    `rust-injection` cargo feature.
  - `src/rust/dictate/backends/inject.rs` — `EnigoInjectBackend`
    implementing the `InjectBackend` trait for `DictateSession` (Wave 5 PR
    5-prep).
- **CLI adapter:**
  - Hidden Phase 1: `inject-text --mode {type|paste} --text ... --xkb-layout ...
    --target-title ... --target-process ...` (cli.rs:93-110) — Wayland
    keycode typing helper the Python worker shells out to.
  - Hidden Phase 2.1: `inject` (cli.rs:235-236) — JSON envelope on stdin,
    JSON on stdout, gated by `VOICEPI_INJECTION_BACKEND=rust`.
  - No public `whisper-dictate type "hello world"` for scripting or
    smoke-testing.
- **Headless testable:** **Partial.**
  - Rust: `injection/*` has unit tests for keymap, wayland ops, fallback
    chain, paste shortcut; `tests/inject_cli.rs` integration test for the
    JSON envelope.
  - Python: `test_injection_dispatch.py`, `test_injection_keymap.py`,
    `test_injection_paste.py`, `test_inject_via_rust.py`. All mock the
    subprocess/enigo boundary.
  - No headless test can exercise a *real* keystroke without a display
    server — inherent limitation.
- **Gap:**
  - No public CLI verb for "inject this text now" (scripting/smoke-test).
  - Python is still the default; Rust is opt-in via env var. The
    `EnigoInjectBackend` trait wire-up into `DictateSession` isn't consumed
    by any production path.
- **Effort estimate:** **small** (add public `inject-text` verb + docs),
  **medium** to flip default, **large** to retire `vp_inject.py`.

---

### 3. Hotkey / PTT (keyboard listener + chord matching)

- **Core logic (Python):** `src/python/whisper_dictate/vp_keys.py` (597 LOC),
  `vp_keys_solo.py` (589 LOC), `vp_keys_capture.py` (228 LOC) —
  pynput + evdev backends, side-aware modifier matching, quit chord, capture
  wizard.
- **Core logic (Rust):**
  - `src/rust/hotkey/mod.rs` (658 LOC) + `hotkey/modifier_match.rs`,
    `hotkey/manager/` (rdev driver + tracker), `hotkey/coordinator/`
    (Stage state machine). Manager/OS layer gated on `--features rust-hotkeys`;
    matching + coordinator compile unconditionally.
  - Runtime installer lives in `src/rust/runtime.rs` (lines 495-555,
    `install_rust_hotkey_from_command`, `restart_hotkey_decision`).
- **CLI adapter:**
  - Python capture wizard: `whisper-dictate run --capture-hotkey`
    (vp_cli.py:286-294) + `vp_keys_capture_cli.py` — the "press-to-capture"
    onboarding flow.
  - Rust hotkey listener has **no** stand-alone CLI verb; it's only wired
    into the `RuntimeSupervisor.start()` path (runtime.rs:443-555) via env-var
    opt-in `VOICEPI_HOTKEY_BACKEND=rust`.
- **Headless testable:** **Partial.**
  - Rust: `hotkey/coordinator/tests.rs`, `hotkey/manager/tracker.rs`
    tests, `hotkey/modifier_match.rs` tests — all pure state-machine
    invariants covered.
  - `runtime/hotkey_supervisor_tests.rs` covers the install/park/resume
    plumbing.
  - Python: `test_keys.py`, `test_keys_capture.py` cover chord matching +
    the capture flow.
  - Real key events cannot be tested headless — inherent (need OS input).
- **Gap:**
  - No `whisper-dictate hotkey capture` / `whisper-dictate hotkey listen`
    public verb — the Python `--capture-hotkey` flag is the only user-facing
    way to run the flow, and it goes through the full Python worker
    bootstrap. Would benefit from a small Rust-native verb that shells to
    the coordinator directly.
  - Python is still shipping default; Rust hotkey path is Wave-5-experimental.
- **Effort estimate:** **small** to add a `hotkey {capture,listen,test-chord}`
  CLI (thin adapter over the coordinator); **large** to flip Rust default and
  retire `vp_keys*.py`.

---

### 4. Dictionary (term substitution + replacements)

- **Core logic (Rust):** `src/rust/dictionary/mod.rs` (282 LOC), plus
  `parse.rs`, `store.rs`, `runtime.rs`, `ops.rs`, `training/*`, `suggest/*`.
  Pure library — no I/O in the core `Dictionary` type. Owns prompt
  construction (`Dictionary::build_prompt`, mod.rs:96), whole-word
  case-insensitive replacements (`Dictionary::apply_replacements`, mod.rs:121),
  and dedupe.
- **Core logic (Python — thin adapter):**
  `src/python/whisper_dictate/vp_dictionary_store.py` (186 LOC),
  `vp_dictionary_training.py` (592 LOC), `vp_dictionary_suggest.py` (509 LOC),
  `vp_dictionary_training_cli.py` (297 LOC). All shell out to Rust via
  `dictionary-ops` (`VOICEPI_DICTIONARY_BACKEND=rust`, `cli.rs:76-79`) or
  keep parity implementations for the default path.
- **CLI adapter (Rust, all public):**
  - `whisper-dictate dictionary status | open | add TERM | replace FROM=TO`
    (cli.rs:277-292).
  - `whisper-dictate dictionary build-from-corpus [--apply --json ...]`
    (cli.rs:298-331) — extracts + dedups terms from the golden corpus.
  - `whisper-dictate dictionary suggest-terms JSONL [--apply ...]`
    (cli.rs:336-353) — mines missed terms from benchmark output.
  - Hidden JSON-RPC: `dictionary-runtime` (cli.rs:71-72), `dictionary-ops`
    (cli.rs:77-79) — used by the Python worker.
- **Headless testable:** **Yes.**
  - Rust: unit tests inside `dictionary/mod.rs` (prompt cap, longest-first
    replace) and every submodule. `training/cli_report.rs`, `suggest/*`
    all covered.
  - Python: `test_dictionary_store.py`, `test_dictionary_training.py`,
    `test_dictionary_training_cli.py`, `test_dictionary_suggest.py`,
    `test_dictionary_rust_backend.py`, `test_dictionary_training_rust_dispatch.py`.
- **Gap:** None material. This module is the reference example of what
  library-first should look like across the rest of the app.
- **Effort estimate:** **already there.** Retiring the Python parity
  implementations of `vp_dictionary_training.py` / `vp_dictionary_suggest.py`
  would trim ~1100 LOC of Python — a nice cleanup follow-up but not a gap.

---

### 5. Model management (whisper GGML catalog + download)

- **Core logic (Rust):** `src/rust/whisper/model_manager.rs` (469 LOC) —
  9-entry catalog, blocking ureq streaming download, SHA-256 verify,
  atomic rename, cache dir, `is_local_only()` gate. Compiled
  unconditionally so the CLI + Settings UI work in stock builds without
  whisper.cpp.
- **Core logic (Rust CLI):** `src/rust/whisper/models_cli.rs` (365 LOC) —
  formatting-only wrapper that calls `model_manager` and prints stable
  lines.
- **CLI adapter (Rust, public):**
  `whisper-dictate models {list,download NAME,path}` (cli.rs:247-267).
- **UI adapter:** `src/rust/ui/whisper_models_state.rs`,
  `src/rust/ui/tabs/whisper_models.rs` — same library calls, task-based.
- **Headless testable:** **Yes.**
  - `whisper/model_manager_tests.rs`, `whisper/models_cli.rs` (unit tests
    in-file for `print_list`), UI-side `ui/model_picker_tests.rs` /
    `ui/whisper_models_state.rs`.
- **Gap:** None. Same reference-quality shape as dictionary.
- **Effort estimate:** **already there.**

---

### 6. Config (load, save, validate, migrate)

- **Core logic (Rust):** `src/rust/config/mod.rs` (161 LOC) with
  `schema.rs`, `settings.rs`, `load.rs`, `save.rs`, `validate.rs`,
  `keys.rs`, `io.rs`. Owns the embedded `settings_schema.json`, typed
  `AppSettings`, `restart_required_keys`, `effective_runtime_env` (the env
  export the Rust supervisor injects into the Python worker at spawn), and
  the platform config-dir resolver.
- **Core logic (Python — compatibility layer):**
  `src/python/whisper_dictate/vp_config.py` (241 LOC) — reads the same
  JSON schema at import for live-reload inside the worker and for direct
  `python -m whisper_dictate.runtime` execution. `settings_schema.json` is
  the single source of truth and shared.
- **CLI adapter:** `whisper-dictate config {path,show}` (cli.rs:270-275).
- **Headless testable:** **Yes.**
  - Rust: extensive unit tests in each `config/*.rs`; the top-of-file
    `mod.rs` test asserts every schema key round-trips through
    `AppSettings::from_value` + `apply_to_object` (mod.rs:100-160).
  - Python: `test_cli_config.py`, `test_cli_config_params.py`,
    `test_settings_schema.py`, `src/tests/python/test_settings_docs_generated.py`.
- **Gap:**
  - No `whisper-dictate config set KEY VALUE` / `unset` / `validate FILE` CLI
    verbs — today users edit `config.json` by hand or through the Settings
    UI.
  - `vp_config.py` remains as a live-reload shim; the worker cannot ask Rust
    for reloaded state today (called out as future work in
    `docs/SOURCE_OWNERSHIP.md:65-66`).
- **Effort estimate:** **small** (add set/unset/validate verbs);
  **medium** to retire `vp_config.py`.

---

### 7. History (dictation log)

**Note:** The mission brief mentioned "SQLite-backed" — the actual
implementation is JSONL, not SQLite. `grep -r sqlite` returns nothing in
`src/`. Correcting the record here.

- **Core logic (Rust):** `src/rust/telemetry.rs` (380 LOC) — `preview_jsonl`,
  `handle_history_command`, `handle_append_history`, `handle_append_jsonl`,
  `handle_append_record_sinks` (batched sink), field allowlist
  (`HISTORY_KEYS`, line 12-43). Path resolution via
  `config::default_history_path`.
- **Core logic (Python — adapter):**
  `src/python/whisper_dictate/vp_history.py` (222 LOC) — shells out to the
  Rust telemetry helpers for every append; only the `--history-*` CLI
  formatting stays Python.
- **CLI adapter:**
  - Rust public: `whisper-dictate history {list [N] | last}` (cli.rs:88-91,
    357-366).
  - Rust hidden RPC (called by Python worker): `append-jsonl --path`,
    `append-history --path`, `append-record-sinks` (cli.rs:147-165).
  - Python-only extras: `--history-copy-last`, `--history-reinject-last`
    (vp_cli.py:267-272) — these need clipboard/injection, so they live in
    Python.
- **Headless testable:** **Yes.**
  - Python: `test_history.py`, `test_benchmark_history.py`.
  - Rust: telemetry has module-level tests; runtime `worker_event_tests.rs`
    exercises the emitter.
- **Gap:**
  - Rust `history` CLI lacks `copy-last` / `reinject-last` because those
    require clipboard + injection (which Rust has now via `paste::Clipboard`
    and `injection::Injector`). Migrating them would let the Rust CLI be a
    superset of Python.
  - Small gap: no `history clear` / `history export --format=csv|md` verbs.
- **Effort estimate:** **small** to add `copy-last`/`reinject-last` to the
  Rust CLI (wire Clipboard + Injector to `telemetry::last_history_entry`).

---

### 8. Prompt building (initial-prompt construction from dictionary)

- **Core logic (Rust):** `Dictionary::prompt_terms` (mod.rs:79-91) +
  `Dictionary::build_prompt` (mod.rs:96-115) — pure functions, budget-aware
  (max_terms + max_chars), returns `Option<String>` so empty base+empty
  vocab yields `None`.
- **Core logic (Python):** `vp_transcribe._dictionary_runtime` /
  `_dictionary_prompt_runtime` (vp_transcribe.py:915-970) shell out to the
  Rust `dictionary-runtime` subcommand (cli.rs:71-72) for the prompt +
  replacements payload.
- **CLI adapter:** hidden `dictionary-runtime` (called per-utterance by the
  Python worker). No public `whisper-dictate prompt build` verb, but the
  full prompt is visible via `whisper-dictate dictionary status`
  (cli.rs:278-280).
- **Headless testable:** **Yes.**
  - Rust unit tests in `dictionary/mod.rs:200-234` cover both cap paths
    (term cap, char cap) and base+vocab composition.
  - Python: `test_dictionary_store.py` exercises the shell-out.
- **Gap:** No public inspector CLI (`prompt build --base "..." --dict PATH`)
  for debugging prompt-vs-model mismatches. `dictionary status` gets close
  but doesn't accept a `--base` override.
- **Effort estimate:** **small.** Add `whisper-dictate dictionary prompt
  [--base TEXT]` verb.

---

### 9. Audio devices (list, select, test microphone)

- **Core logic (Python):** `src/python/whisper_dictate/vp_devices.py`
  (561 LOC) — PortAudio/sounddevice enumeration, WASAPI/DirectSound/MME/WDM-KS
  duplicate collapsing, name matching. Called by
  `--list-audio-devices` (vp_cli.py:211-217) and by the Settings UI.
- **Core logic (Python):** `vp_device_test.py` (280 LOC) — the
  "dry-run open the mic" test that mirrors the same WASAPI open matrix as
  capture.
- **Core logic (Rust — parallel):** `src/rust/devices.rs` — cpal-based
  enumerator with the same JSON envelope shape the Settings UI reads.
  Gated on `--features audio-in-rust`. Called via
  `VOICEPI_DEVICES_BACKEND=rust` (cli.rs:239-241, `whisper-dictate devices`).
- **UI:** `src/rust/ui/audio_devices.rs`, `ui/device_test.rs`,
  `ui/audio_device_picker_tests.rs` (unit-tested picker logic).
- **CLI adapter:**
  - Rust public: `whisper-dictate devices` (cli.rs:239-241) — JSON list.
  - Python: `whisper-dictate run --list-audio-devices`,
    `--test-audio-device NAME` (vp_cli.py:211-217, 213-217).
  - **No Rust CLI for the microphone-open probe** — that's Python-only.
- **Headless testable:** **Partial.**
  - Python: `test_audio_device_listing.py`, `test_device_test_mode.py`,
    `test_devices_rust_backend.py`.
  - Rust: `ui/audio_device_picker_tests.rs` covers the picker; devices.rs
    itself has unit tests around the enumeration shape but real cpal
    enumeration is host-dependent.
  - **Cannot** headless-test opening a real mic device (no audio hardware
    on CI Linux runners; PulseAudio dummy sink is a workaround, not tested).
- **Gap:**
  - Rust has no `devices test NAME` (dry-open probe) verb.
  - Python `vp_devices.py` is 561 LOC of PortAudio glue that will need to
    move to Rust or stay Python-owned depending on how far
    `--features audio-in-rust` graduates.
- **Effort estimate:** **medium** to port `vp_device_test.py` to Rust (the
  cpal open-config matrix mirrors what Python does with sounddevice);
  **large** to retire `vp_devices.py` and cut over the Settings UI default.

---

### 10. UI (settings tab, tray, overlay, onboarding)

By nature this is egui rendering — library-first doesn't apply to the pixel
push. However, the module is structured with a **large pure-logic
substrate** that is exactly the right shape:

- **Pure predicates/helpers (unconditional compile, unit-tested):**
  - `ui/tray.rs` — `tray_state_for`, `tray_state_for_capture`,
    `tray_icon_rgba` (cfg-free, tested on every platform).
  - `ui/tabs/runtime_format.rs` — `format_push_to_talk_keys`, mic-label
    budget, gauge colours, log summariser.
  - `ui/tabs/top_status_layout.rs` — width-budget layout, post-indicator
    labels.
  - `ui/whisper_models_state.rs`, `ui/tasks.rs`,
    `ui/settings_state.rs`, `ui/model_capacity` view-model.
  - Many `*_tests.rs` siblings: `audio_device_picker_tests.rs`,
    `benchmark_task_tests.rs`, `keyboard_layout_tests.rs`,
    `layout_tests.rs`, `model_picker_tests.rs`, `robustness_tests.rs`,
    `runtime_status_tests.rs`, `settings_reset_tests.rs`,
    `tab_helpers_tests.rs`, `ui_language_tests.rs`, `update_check_tests.rs`,
    `worker_json.rs` + tests.
- **Onboarding:** implemented UI-side via egui::Modal wizard (see recent
  commit d4ed78f "fix(onboarding): use egui::Modal + repaint-on-advance to
  unstick wizard"). Python has `vp_setup.py` (582 LOC) for the CLI
  `--setup` wizard equivalent.
- **CLI adapter:** `whisper-dictate ui` / `whisper-dictate settings`
  (cli.rs:19-21). Windows-only tray behaviour behind `#[cfg(windows)]`
  gate.
- **Headless testable:** **Yes for the substrate**, **no for the pixels**.
  The rich `*_tests.rs` coverage handles predicates; rendering can only be
  smoke-tested (`test_egui_boundary.py`).
- **Gap:** small — a smoke `whisper-dictate ui --render-once` verb that
  runs one repaint and exits would let a headless CI leg catch panics in
  the egui layer without an X server (currently only surfaces at runtime).
- **Effort estimate:** **anti-recommendation** — see below. The substrate
  is already testable; deeper pixel-diffing isn't worth the harness cost.

---

### 11. Runtime orchestration (start/stop worker, wire config to session)

- **Core logic (Rust):** `src/rust/runtime.rs` (2224 LOC — the elephant).
  `RuntimeSupervisor` owns child-process lifecycle, worker-event parsing,
  audio-bridge readiness gating, Rust-hotkey install/park/resume decisions,
  session-sink teardown, `WorkerCommand` building
  (`worker_command_with_args`, line 1275), `parse_toggle_value`,
  `normalise_hotkey_chord_for_python`, and a large family of
  `#[cfg(feature = "audio-in-rust")]` bridge management.
- **Core logic (Python):** `src/python/whisper_dictate/runtime.py`
  (830 LOC) + `vp_dictate.py` (833 LOC) — the actual PTT event loop lives
  in `vp_dictate.py::Dictate`; `runtime.py::main` is the entry point.
- **Core logic (Rust — pure-logic port, not yet consumed):**
  `src/rust/dictate/session/` — the full per-utterance state machine mirrors
  `vp_dictate.py::Dictate` end-to-end with `TranscribeBackend` /
  `InjectBackend` traits (`session/types.rs`, `session/mod.rs`). Real
  backend impls exist in `dictate/backends/` behind `whisper-rs-local` /
  `rust-injection`. `dictate/audio_route/` bridges cpal events into the
  session. **No production caller today**; this is Wave 5 PR 2-5 work
  landing incrementally.
- **CLI adapter:**
  - Public: `whisper-dictate run [-- args]` (cli.rs:22-27) —
    delegates to `runtime::run_terminal` → spawns
    `python -m whisper_dictate.runtime`.
  - Public: `whisper-dictate ui | settings` (spawns egui, which spawns the
    supervisor).
  - Hidden: `dictate-ops` (cli.rs:82-86) — Rust exposes the pure-logic
    helpers (skip decision, restart-required keys, backend label) as JSON
    RPC for the Python worker to shell to.
- **Headless testable:** **Partial.**
  - Rust: `runtime/*_tests.rs` (18 test files!) cover install plan, worker
    command building, hotkey supervisor, bridge terminal, state
    transitions, session sink, ubuntu setup, windows process helpers.
    `dictate/session/tests_ported.rs` ports the six characterisation
    tests from `test_dictate_loop.py`.
  - `src/rust/tests/runtime_supervisor.rs` integration test.
  - Python: `test_dictate.py`, `test_dictate_loop.py`,
    `test_dictate_rust_backend.py`, `test_e2e_pipeline.py`,
    `test_rust_boundary.py`.
- **Gap (biggest in the audit):**
  - Two parallel dictation orchestrators exist. Python's owns the shipping
    hot path; Rust's `DictateSession` is complete but only wired in a
    Wave-5 PR chain that has landed in pieces but not yet flipped default.
  - `runtime.rs` at 2224 LOC violates the 500-LOC modularity rule (memory
    entry `code-modularity-rule`). Splitting the audio-bridge management
    (~600 LOC of `#[cfg(feature = "audio-in-rust")]`) and the hotkey
    install/restart decision tree (~200 LOC) into submodules would help
    future work.
  - No `whisper-dictate session step ...` sort of CLI to drive the state
    machine from a script/test harness — headless CI has to go through the
    full supervisor.
- **Effort estimate:** **large.** This is the wave-8 endgame of #348.

---

## Prioritised refactor list

Ranked by (value × ease). "Value" weighs test-lever pull, CLI addressability,
and enabling downstream retirement of Python parity code.

### 1. Split `runtime.rs` into modules (audio-bridge, hotkey-supervisor, worker-command)
**Why now:** Enforces the 500-LOC modularity rule (memory
`code-modularity-rule`), unlocks separate test files without cross-import
grief, and *doesn't* require any behaviour change — pure structural
refactor. Also makes the eventual wave-8 cut-over across `DictateSession`
much easier to review.
**Deliverable:** `runtime/mod.rs` re-exporting current public API;
sub-modules `runtime/supervisor.rs`, `runtime/audio_bridge.rs`
(cfg-gated), `runtime/hotkey_install.rs`, `runtime/worker_command.rs`.
**Effort:** ~1-2 days.

### 2. Public Rust CLI verbs for one-shot ops (`inject-text`, `hotkey capture`, `config set`, `dictionary prompt`, `history copy-last/reinject-last`, `devices test`)
**Why now:** Every one of these is a thin wrapper over an already-existing
Rust library function. Ships CLI addressability for six features in one
sweep; enables shell-scripted smoke tests in the Ubuntu 26.04 CI container
(see recommended setup below).
**Deliverable:** ~150 LOC across `cli.rs` + six handler stubs plus tests.
**Effort:** ~1 day per verb group; ~3-4 days total.

### 3. Port `vp_device_test.py` (mic-open probe) to Rust; add `whisper-dictate devices test NAME`
**Why now:** The cpal open-config matrix in `src/rust/audio/capture.rs`
already exercises the same negotiation path as
`vp_device_test._probe_device`. Moving it to Rust closes the last gap
in the "devices" feature and starts to justify defaulting
`VOICEPI_DEVICES_BACKEND=rust`. Also enables headless "does the mic even
open" checks in CI containers with a PulseAudio dummy sink.
**Deliverable:** `src/rust/devices.rs::test_open(name)` + `devices test`
subcommand + unit tests.
**Effort:** ~2 days (needs cross-platform host-specific config matrix work).

### 4. Retire Python parity implementations of dictionary training / suggest
**Why now:** Rust `dictionary/training/*` + `dictionary/suggest/*` are
complete and covered; `vp_dictionary_training.py` (592 LOC) and
`vp_dictionary_suggest.py` (509 LOC) exist only as parity fallbacks. Cutting
`VOICEPI_DICTIONARY_BACKEND=rust` to the default and removing the Python
files reclaims ~1100 LOC, reduces Python test surface, and makes
"dictionary" the fully-realised reference feature for the library-first
pattern.
**Deliverable:** delete `vp_dictionary_training.py`,
`vp_dictionary_suggest.py`, `vp_dictionary_training_cli.py`; keep
`vp_dictionary_store.py` as a shim; update tests that hit Python
implementations.
**Effort:** ~2-3 days (mostly test updates and verifying no import remnants).

### 5. Wire `dictate::DictateSession` into the shipping PTT loop (behind an env flag first)
**Why now:** Highest strategic value, hardest work. All the pieces exist:
`DictateSession` state machine (Wave 5 PR 2), audio-route bridge (PR 3),
hotkey wiring (PR 4), real backends (PR 5). The final swap is what closes
the "runtime orchestration" gap. Doing this after (1) and (2) means the
refactor lands into a clean `runtime/*` module tree and can be driven from
CLI for CI testing.
**Deliverable:** flip `VOICEPI_DICTATE_BACKEND=rust-session` from
opt-in to opt-out (default on) once the Wave-5 PR chain is in `main`; keep
Python worker as an emergency fallback; new integration test that runs
one utterance end-to-end from a synthetic AudioPipeline event stream to a
mocked injector.
**Effort:** ~1-2 weeks including the required test-harness work and the
release-matrix change to always build with
`--features audio-in-rust,whisper-rs-local,rust-injection,rust-hotkeys`.

---

## Recommended container test setup

Target: Ubuntu 26.04 dev container that runs on every PR to give a
"library-first" green light. The existing `.devcontainer` (per memory
`local-dev-environment`) already covers most of this.

**Install (apt):**
- `build-essential`, `pkg-config`, `cmake` (for whisper.cpp behind
  `whisper-rs-local`)
- `libasound2-dev` (cpal; already present as
  `libasound2-dev_1.2.11-1ubuntu0.2_amd64.deb` in the git status)
- `libx11-dev`, `libxi-dev`, `libxtst-dev` (enigo on X11)
- `libgtk-3-dev`, `libayatana-appindicator3-dev` (optional, for tray parity)
- `xvfb`, `pulseaudio-utils`, `pulseaudio-module-null` (headless display +
  fake audio sink)
- `python3.12`, `python3.12-venv`, `pip` (per Python-side tests)

**Runs green in that container:**
- `cargo test --features "audio-in-rust,whisper-rs-local,rust-injection,rust-hotkeys"`
  — every `*_tests.rs`, every `src/rust/tests/*.rs`.
- `pytest src/python/tests src/tests/python` — the full Python suite; the
  Rust-backend variants (`test_*_rust_backend.py`) shell to the built
  binary.
- `whisper-dictate models list` / `models download tiny.en` (network-gated;
  optional slow leg).
- Any new one-shot CLI verbs from prioritised item (2) — smoke each one
  and assert exit=0 + expected stdout key.
- `whisper-dictate devices` (via PulseAudio null sink, cpal enumerates it).
- `whisper-dictate dictionary build-from-corpus` against the checked-in
  corpus.
- `whisper-dictate bench` (against the golden corpus, feature-gated on
  whisper-rs-local + a cached tiny.en).

**Cannot test in that container (inherent limitations):**
- Real keystrokes injected into a real focused window (no user-focused
  Wayland/Windows target).
- Real hotkey capture (no OS input events beyond synthetic X events under
  Xvfb; the coordinator/tracker unit tests already cover matching, which
  is what usually breaks).
- Windows tray behaviour (`#[cfg(windows)]`); needs a Windows CI leg —
  which the release matrix already provides.
- macOS accessibility permission dialogs.
- Real GPU/CUDA transcription (VRAM absent; CPU path only in CI).
- Wayland-specific injection (`wtype`, `kwtype`, `dotool`, `ydotool`) —
  these need a real Wayland compositor, which Xvfb doesn't provide.

**Metric to shoot for:** a "library-first" CI leg that runs every Rust
`cargo test` + every Python `pytest` in under 5 minutes on the container,
and calls each public CLI verb once via a smoke script that just checks
exit codes and top-level JSON keys.

---

## Anti-recommendations

Features / modules where library-first refactor is **not** worth the
effort:

- **egui rendering pixels** (`src/rust/ui/tabs/*` visual layer, `ui/app.rs`
  draw code). The pure predicates and layout helpers already live in
  `runtime_format.rs`, `top_status_layout.rs`, `tray.rs` etc. and are
  fully unit-tested. Pixel-diff harnesses for egui add a large maintenance
  burden for near-zero regression capture — visual bugs are almost always
  caught by the "run the app once" smoke path.

- **`vp_transcribe.py` hallucination filter and decode-temperature ladder**
  (~200 LOC of Python regexp state around faster-whisper's segment API).
  Moving this to Rust means either duplicating in Rust and keeping in sync,
  or gating on `whisper-rs-local` fully — neither pays off until item (5)
  above is in flight. Leave as Python parity code until then.

- **`vp_capture.py` sounddevice path** (1149 LOC). The Rust `audio::`
  pipeline is the strategic replacement; investing further in a Rust
  library port of the sounddevice-specific code is wasted effort. When the
  cpal path ships as default, `vp_capture.py` should be deleted, not
  refactored.

- **Windows tray icon rendering** (`ui/tray.rs::TrayManager` behind
  `#[cfg(windows)]`). The pure state machine is unit-tested on Linux CI;
  wrapping the actual `tray-icon` crate calls in a testable façade would
  cost more than it prevents (the crate is small and well-vetted).

- **`runtime.py::main` bootstrap** (Windows CUDA DLL discovery, HF cache
  suppression, PYTHONIOENCODING dance). All of that is Python-VM specific
  glue; moving it to Rust would just move the same platform quirks to
  Rust and force new IPC. Keep it in the Python entrypoint until the
  entire Python worker retires.

---

## Appendix: architectural insights / surprises

1. **History is JSONL, not SQLite.** The mission brief said SQLite; the
   codebase has zero sqlite/`.db` references. `vp_history.py` uses JSONL
   via the Rust `telemetry` helpers. If SQLite migration is intended as a
   future item, treat it as a separate proposal — the current shape is
   fine for now.

2. **The Wave-5 PR chain is remarkably clean but not connected.** The Rust
   `dictate::session::DictateSession` state machine, the audio-route
   bridge, the real Whisper/enigo backends, and the coordinator wiring are
   all in `main`, individually reviewed, individually tested — and yet
   the shipping worker is unchanged because the final "swap in real
   backends" PR hasn't landed. This is by design (small reviewable
   increments) but means the production code path lags the library
   capabilities by a full wave.

3. **`runtime.rs` at 2224 LOC is the only violator of the modularity
   rule** in the Rust tree. Every other module is diligently split. This
   file has quietly grown; splitting it should be item (1) precisely
   because it's the file everyone touches when they land wave work.

4. **The `docs/SOURCE_OWNERSHIP.md` table is authoritative and up to date.**
   It essentially pre-answers the "who owns what" half of this audit. The
   audit's gap list is mostly agreement with the "Good Rust candidates"
   section of that doc plus the concrete CLI-verb gaps.

5. **CLI addressability is already very good.** 26 subcommands and
   nested-subcommands in `cli.rs`, with 13 of them hidden JSON-RPC RPCs
   the Python worker shells to. The gap is not "Rust needs a CLI" — it's
   "one-shot operator/scripting verbs like `inject-text HELLO` or
   `history copy-last`" that would let a human/CI drive individual
   features without spawning a worker.

6. **Every feature already has some form of headless test.** The Python
   test suite has 60+ files; Rust has 40+ `*_tests.rs` files plus 5
   integration tests under `src/rust/tests/`. The gap is not "we don't
   test" — it's "some tests still mock the boundary they should be
   driving through a real library call" (mostly on the audio/capture and
   PTT event-loop side).

---

## Quick lookup — feature → primary file(s)

| Feature | Rust core | Python core | CLI |
|---|---|---|---|
| Transcription | `whisper/dispatch.rs`, `whisper/local/mod.rs`, `cloud_api/transcribe.rs`, `dictate/backend.rs` | `vp_transcribe.py`, `vp_external_api.py` | `cloud-transcribe`, `transcribe-wav`, `transcribe-server` (hidden) |
| Injection | `injection/mod.rs`, `dictate/backends/inject.rs` | `vp_inject.py`, `vp_inject_rust.py` | `inject-text`, `inject` (hidden) |
| Hotkey/PTT | `hotkey/mod.rs`, `hotkey/coordinator/`, `hotkey/manager/` | `vp_keys.py`, `vp_keys_solo.py`, `vp_keys_capture*.py` | run `--capture-hotkey` (Python) |
| Dictionary | `dictionary/mod.rs` + submodules | `vp_dictionary_store.py` (adapter) | `dictionary {status,open,add,replace,build-from-corpus,suggest-terms}` |
| Model mgmt | `whisper/model_manager.rs`, `whisper/models_cli.rs` | — | `models {list,download,path}` |
| Config | `config/mod.rs` + submodules | `vp_config.py` (compat) | `config {path,show}` |
| History | `telemetry.rs` | `vp_history.py` (adapter) | `history {list,last}`, `append-*` (hidden) |
| Prompt building | `dictionary::build_prompt` in `dictionary/mod.rs` | `vp_transcribe._dictionary_runtime` | `dictionary status` (indirectly), `dictionary-runtime` (hidden) |
| Audio devices | `devices.rs` (feature-gated) | `vp_devices.py`, `vp_device_test.py` | `devices` |
| UI | `ui/*` (egui) | — | `ui`, `settings` |
| Runtime orchestration | `runtime.rs`, `dictate/session/`, `dictate/audio_route/`, `dictate/backends/` | `runtime.py`, `vp_dictate.py` | `run`, `ui`, `dictate-ops` (hidden) |
