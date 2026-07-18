<!--
Design doc for audit item 5 Phase B (see
`docs/design/item5-wire-dictate-session.md` for the Phase A/B/C
blueprint). Planning artefact only — no code, no timeline commitments
beyond the estimates at the end. Author: item-5 Phase B planning
subagent, 2026-07-18.
-->

# Item 5 Phase B: in-process Rust dictation dispatch

## Recap of Phase A

Phase A shipped in v1.22.0-rc.1 (commits `364edbc` + `21e8690`; see
release commit `40a7698`). It threaded a new opt-in env var,
`VOICEPI_DICTATE_ENGINE`, through the Python worker's session dispatch
and — when the operator sets it to `rust` — shells out to a new Rust
CLI verb instead of running the in-process Python PTT loop.

Concretely:

- **New CLI verb.** `whisper-dictate dictate-run` at
  `src/rust/runtime/dictate_run.rs:64` installs the full Rust dictation
  runtime (hotkey listener + coordinator + session sink + real backends
  when compiled in) and runs until Ctrl-C. Emits a
  `{"ready":true,"engine":"rust"}` gate line on stdout when
  `--json-events` is passed so a supervising parent can wait for
  first-fire before assuming success (`dictate_run.rs:264-283`).
- **Python dispatch shim.**
  `src/python/whisper_dictate/vp_dictate_engine.py:159`
  (`run_rust_engine`) spawns the verb, forwards each stdout line to the
  Python parent's stdout, and returns `(ran, code)`. If the child never
  emits the ready signal it falls back to Python
  (`vp_dictate_engine.py:207-245`).
- **Wire-up point.** `_dispatch_engine` at
  `src/python/whisper_dictate/runtime.py:754-802` branches on the env
  var before constructing `Dictate(...).run()`; the whole existing
  Python PTT loop is untouched when the flag is unset or set to
  `python`.

**Why subprocess-first was right.** Blast-radius. The Python worker's
`_dispatch_engine` still owns model loading, argparse, config plumbing,
and the KeyboardInterrupt/exit-code contract with the Rust
supervisor. A subprocess boundary meant we could ship Rust dictation
end-to-end without touching any of that. A crash in the Rust child
became `(False, code)` and a documented fallback, not a whole-worker
outage. v1.20.x's failure mode (see the reference doc's regression
table) was exactly the outcome subprocess-first avoids.

Post-A architecture — three processes when the user opts in:

```text
whisper-dictate UI  ──spawn──►  python -m whisper_dictate.runtime  ──spawn──►  whisper-dictate dictate-run
   (Rust supervisor,               (Python worker; loads model, then                 (Rust child; hotkey +
    hotkey via VOICEPI_             calls run_rust_engine which forwards              coordinator + session
    HOTKEY_BACKEND=rust)            JSON events verbatim to its own stdout)           + real backends)
```

## Why Phase B

Phase A's win is *safety*, not performance. The measurable costs Phase
B removes are:

1. **Dual-language state coordination.** Today the Python parent
   forwards stdout lines opaquely
   (`vp_dictate_engine.py:138-142`). Every new supervisor event or
   worker-event field needs a Python passthrough test in
   `src/python/tests/test_dictate_engine_dispatch.py` even though
   nothing Python-side reads the payload. The transitive contract is
   Rust → Python → Rust supervisor for zero Python consumers.
2. **Two ready-signal ladders.** The Rust supervisor still waits on
   the Python `state=ready` stderr event
   (`supervisor.rs:326-368` in the `audio-in-rust` path). The Python
   worker then waits on the Rust child's `{"ready":true}` JSON line
   (`vp_dictate_engine.py:207-221`). A user-visible PTT press has to
   traverse both. Phase B collapses them.
3. **Failure-model complexity.** Three-process pipelines have three
   independent exit codes and three independent stderr sinks. The
   `dictate-run` child inherits stderr but its `--json-events` stdout
   goes through a Python line-buffer
   (`vp_dictate_engine.py:125-135, 208-221`). A partial write during
   shutdown races the Python parent's `proc.wait(timeout=10)`. This
   is fixable but not worth fixing if Phase B removes the pipeline.
4. **Two model loads.** With `VOICEPI_DICTATE_ENGINE=rust` the Python
   worker still runs `_load_model` before `_dispatch_engine`
   (`runtime.py:877-878`). If the Rust child ALSO loads a whisper
   model (via `whisper-rs-local` when compiled in), we pay for the
   model twice on cold start. Phase B lets us skip Python's load path
   entirely.

**What Phase B is NOT about.** Per-utterance latency. The subprocess
`fork+exec` cost is paid once at engine start, not per press —
`dictate-run` runs the event loop until Ctrl-C
(`dictate_run.rs:202-216`). The user's Phase B brief flagged this and
was right; if this section had led with latency it would be
misleading.

## Design

### The three plausible options

#### Option 1: Rust supervisor becomes the launcher (recommended)

The Rust supervisor at `src/rust/runtime/supervisor.rs:175` branches
on `VOICEPI_DICTATE_ENGINE=rust` *before* building the Python
`WorkerCommand`. When set, it constructs the same production sink
Phase A's `dictate-run` verb constructs
(`rust_session_sink::build_production_sink`,
`src/rust/runtime/rust_session_sink.rs:267`) and installs the hotkey
directly. No Python worker child is spawned.

Concretely:

- Extract the body of `dictate_run::run`
  (`src/rust/runtime/dictate_run.rs:98-224`) into a reusable
  `InProcessDictationRuntime` type that can be installed into either a
  standalone process (the `dictate-run` verb) OR a hosting supervisor
  (the UI process).
- In `RuntimeSupervisor::start`, branch: if the engine flag is `rust`,
  install the runtime in-process; otherwise fall through to today's
  Python-worker spawn.
- The UI's `worker_command` path (`src/rust/ui/app.rs:305`) stays
  untouched; the branch lives one layer down.

This is close to what `VOICEPI_DICTATE_BACKEND=rust-session` already
does (`src/rust/runtime/hotkey_install.rs:362-391`) — the difference
is that `rust-session` keeps the Python worker child spawned as a
logging/status conduit; Phase B drops that child.

**Failure model.** A panic in the in-process session aborts the whole
UI process. Mitigation is the `VOICEPI_DICTATE_ENGINE=python` env-var
escape hatch preserved from Phase A: setting it forces the old
subprocess path, restoring the Python-worker child and the two-layer
supervision.

#### Option 2: Python worker imports Rust via ABI (rejected)

Expose the Rust dispatcher as a C ABI or PyO3 module. Python
`_dispatch_engine` calls into it in-process.

Rejected because:

- Adds a new ship artefact (a `.so`/`.dll`/`.dylib` per platform) to
  the Windows/nix/choco/winget/brew matrix. The reset that produced
  v1.21.0 was partly about *reducing* moving parts.
- PyO3 pins the ABI to a specific CPython minor version; every Python
  update becomes a rebuild.
- Doesn't remove the two-model-load problem (Python still loads its
  model before the ABI hand-off unless we rewire `_load_model` too —
  and if we rewire that, we're most of the way to Option 1 anyway).
- Doesn't remove the Python worker process from the supervision
  ladder. The Rust supervisor still `Popen`s Python, which still hosts
  the loop — just with a Rust core inside it. The failure surface
  grows (segfaults now cross the FFI boundary) without shrinking the
  operational surface.

#### Option 3: Python worker as a thin adapter (rejected)

Keep Python as a startup shim: load config, resolve model path, then
`os.exec` into `whisper-dictate dictate-run`.

Rejected because:

- Still runs Python at startup, so cold-start cost is unchanged.
- The Python surface doesn't shrink — it just gets weirder (why is
  there a Python program whose only job is to launch a Rust program
  the launcher already knows how to run?). Fair criticism at PR time.
- The Rust supervisor already resolves config paths and knows the app
  root (`src/rust/runtime/worker_command.rs:78-124`); this option
  duplicates that logic in Python.

### Recommendation

**Option 1.** It's the smallest step that actually removes Python from
the runtime loop when the flag is set, it composes cleanly with the
existing `rust-session` sink code path (much of Option 1 is *renaming*
the gate variable, not writing new code), and it preserves the Phase A
env-var escape hatch for the user's manual fallback.

## Migration path

### What must exist in Rust before Phase B

Most of it already exists in some form:

- **Hotkey coordinator + install** — `src/rust/hotkey/` +
  `src/rust/runtime/hotkey_install.rs` (used by Phase A `dictate-run`
  and the supervisor's optional Rust-hotkey wiring).
- **Production session sink** — `build_production_sink` at
  `src/rust/runtime/rust_session_sink.rs:267` already picks real
  backends when `whisper-rs-local` + `rust-injection` are compiled in,
  otherwise stubs (`rust_session_sink.rs:289-338`).
- **Real backends** — `src/rust/dictate/backends/whisper_local.rs`
  (transcribe), `src/rust/dictate/backends/inject.rs` (Enigo), and
  `src/rust/runtime/rust_session_real_backends.rs` (wire-up).
- **Config load** — `crate::config::{load_settings,
  load_settings_from_path}` used by `dictate_run.rs:123-127`.

The new work is the extraction: turn `dictate_run::run` into an
`InProcessDictationRuntime::install_into(&mut supervisor)` shape that
can be driven from `RuntimeSupervisor::start` without going through
`Command::new`. This is refactor-scoped, not new-code-scoped, and the
existing `dictate-run` verb becomes a thin wrapper around the same
type so its regression coverage stays live.

### What Python code becomes dead

Once Phase B's default becomes `rust` (Phase C in the reference doc):

- `src/python/whisper_dictate/vp_dictate_engine.py` (the whole file —
  its only reason to exist is dispatching to the subprocess).
- The `_dispatch_engine` branch in `runtime.py:754-790`.
- The `run_rust_engine` re-export in `runtime.py:149-152`.
- The tests at `src/python/tests/test_dictate_engine_dispatch.py`.

Delete follows the item-4 retire pattern (a follow-up PR after bake).
Phase B itself does NOT delete Python — the Phase A subprocess path
stays as an in-process fallback until Phase C.

### Env-var strategy

Reuse `VOICEPI_DICTATE_ENGINE` unchanged. `python` (default), `rust`
(in-process from Phase B onward). Users flipping between v1.22.0-rc.1
and v1.22.0 keep the same knob; the behaviour it triggers goes from
"subprocess a Rust child from the Python worker" to "Rust supervisor
skips the Python worker entirely" without a rename. This matters
because release notes, support threads, and the wayland-user-smoke
script already reference the name
(`scripts/integration/wayland-user-smoke.sh:601-624`).

**Sub-note on `VOICEPI_DICTATE_BACKEND=rust-session`.** That env var
(`src/rust/runtime/rust_session_sink.rs:53`) already installs the
in-process session sink *alongside* a Python worker child. Phase B
should NOT introduce a third env var; instead, when
`VOICEPI_DICTATE_ENGINE=rust` is set, the supervisor implicitly acts
as if `rust-session` were set AND skips the Python child. This
consolidates the two overlapping opt-ins into one flag so users don't
have to know both.

## Prereqs

- **Phase A production-tested.** v1.22.0-rc.1 shipped; need at least
  the two-week prerelease bake window called out in the reference
  doc's Phase A row. No known regressions from Phase A user testing
  must be open.
- **`dictate-run` refactor into `InProcessDictationRuntime`.** The
  extraction should land as its own PR *before* the supervisor
  branch, so the diff review can focus on "does this still install
  the same runtime?" without a supervisor change in the same commit.
- **Rust supervisor can load the same effective config as the Python
  worker.** Today the supervisor builds a `WorkerCommand` whose env
  Python inspects at startup (`worker_command.rs:82-124`). Phase B
  needs the supervisor itself to resolve `settings.key`,
  `settings.toggle_mode`, and audio-device settings via
  `config::load_settings`. The `dictate-run` verb already does this
  correctly (`dictate_run.rs:122-143`); the risk is drift between the
  Python-side resolver and the Rust-side one (see Risks).
- **Auto-fallback plumbing.** If the in-process runtime fails to
  install (missing features, config error), the supervisor MUST fall
  back to spawning the Python worker with `VOICEPI_DICTATE_ENGINE`
  cleared for that process, and emit a `RuntimeEvent::Stderr` line
  operators can grep. Mirror the existing Phase A pattern at
  `vp_dictate_engine.py:181-207`.

## Test plan

**Common baseline (unchanged from Phase A):**

- `cargo test --features
  "audio-in-rust,whisper-rs-local,rust-injection,rust-hotkeys"` green.
- `pytest src/python/tests src/tests/python` green with
  `VOICEPI_DICTATE_ENGINE` unset AND with `=python` explicitly set.

**Phase B additions:**

- **In-process install smoke.** New Rust integration test:
  `RuntimeSupervisor::start` with `VOICEPI_DICTATE_ENGINE=rust` in
  env; assert `is_running()` is true AND no child process was spawned
  (extend `src/rust/tests/runtime_supervisor.rs`; the existing
  `src/rust/runtime/bridge_terminal_tests.rs:73-84` is a template).
- **Ready-signal parity.** With the in-process runtime installed, the
  supervisor MUST emit the same `RuntimeEvent::Worker { event:
  "state", state: Some("ready"), ... }` event that the Python worker
  emits, so the UI's ready-latch (`src/rust/ui/app.rs`, search for
  `worker_ready`) fires identically. Regression test drives
  `Supervisor::poll()` after `start()` and asserts the event.
- **Fallback-on-install-failure.** Simulate a config-load failure
  (invalid `settings.key`) with `VOICEPI_DICTATE_ENGINE=rust`; assert
  the supervisor falls back to spawning Python and surfaces one
  `RuntimeEvent::Stderr` line naming the reason.
- **Phase A subprocess-path regression.** Set
  `VOICEPI_DICTATE_ENGINE=python` explicitly; assert the exact
  three-process pipeline still works — this guarantees the Phase A
  code path remains a live fallback (not the default anymore, but
  present).
- **The 5-press regression harness from the reference doc's Phase B
  row** must run in both modes (`=rust` in-process AND `=python`
  subprocess), not just one.

**Cannot test in CI (must be manual, blocks RC → final promotion):**

- Real keystroke injection into a real user-focused window on
  Windows/Wayland/macOS with the in-process runtime installed.
- Cold-start latency comparison (in-process vs. subprocess) on a real
  user machine — a talking point for release notes.

## Risks

1. **Config-parsing parity with Python.** The Python worker's config
   resolution goes through `apply_config_to_environ()` and several
   downstream `get_value()` calls (see `runtime.py:844` and the
   surrounding lines). `dictate_run::run` uses the Rust-side
   `config::load_settings` (`dictate_run.rs:122-127`); any drift
   between the two resolvers produces a config that installs under
   Python but silently misbehaves under Rust. Mitigation: add a test
   that loads a synthetic config under BOTH resolvers and asserts
   equality on the fields Phase B actually consults (`key`,
   `toggle_mode`, audio device selection, injection backend
   selection).
2. **UI-to-supervisor status plumbing.** Today the UI reads
   `RuntimeEvent::Worker` events streamed from the Python child's
   stderr via `stream_lines`. The in-process runtime must emit the
   same events on the same `Sender<RuntimeEvent>`; the existing
   `rust_session_sink` code does this
   (`rust_session_sink.rs:296-325`), but the ready-event and the
   audio-device-active event are emitted by DIFFERENT paths in Python
   than in Rust. Mitigation: catalogue the events the UI observes
   (grep `worker_ready`, `audio_capture_active` in `src/rust/ui/`)
   BEFORE the extraction PR, and add each to a
   `dictate_run_events_parity` test.
3. **Panic containment.** A panic in the in-process runtime kills the
   UI process. Today (Phase A) a panic in `dictate-run` only kills
   the Rust child, and the Python parent surfaces
   `subprocess exited without READY signal` and returns to Python.
   Mitigation: `catch_unwind` at the supervisor-side install boundary
   for the in-process runtime, converting a panic into a
   `RuntimeEvent::Error` + fallback to the Phase A subprocess path
   for the remaining process lifetime.
4. **Model-load UX regression.** The Python worker prints
   `model ready in Ns` (`runtime.py:710`) that users see as the
   Settings tab's readiness signal. If the in-process runtime loads
   the model on the UI thread it will freeze the UI for 3-8 s on cold
   start. Mitigation: keep the existing `rust_session_real_backends`
   pattern that constructs backends up-front but on the supervisor's
   own thread, and emit a `state=loading` worker event before the
   load blocks. This is the same UX shape the reference doc's Phase B
   risk 5 flagged.
5. **Env-var precedence surprise.** A user with both
   `VOICEPI_DICTATE_ENGINE=rust` AND
   `VOICEPI_DICTATE_BACKEND=rust-session` in their environment is
   effectively asking for two overlapping in-process paths. Phase B
   should treat `ENGINE=rust` as authoritative and log an
   informational line naming the effective backend so the operator
   knows which one won. This is nomenclature debt from having two
   env vars for one concept.

## Estimate

Same format as the Phase A doc's estimate table. Engineering-day
figures assume one senior engineer full-time with normal review
cycles.

| Sub-phase | Eng days | Calendar days (incl. bake) |
|---|---|---|
| Extract `InProcessDictationRuntime` from `dictate_run::run` (refactor PR) | 2-3 | 3-5 |
| Supervisor branch on `VOICEPI_DICTATE_ENGINE`; add fallback + panic guard | 3-4 | 4-6 |
| Test coverage (in-process install, ready parity, fallback, 5-press harness in both modes) | 3-4 | 5-7 |
| Manual real-user testing on Windows + Wayland + macOS + RC bake | 1-2 | 10-14 (2-week prerelease bake) |
| **Phase B total** | **9-13** | **~22-32 days end-to-end** |

The Phase C retire (Python worker deletion) is unchanged from the
reference doc's estimate — it still needs the 30-day zero-fallback
bake gate.

## Non-goals

- **Deleting the Phase A subprocess path.** Phase B keeps
  `run_rust_engine` and the `dictate-run` verb alive as the
  fallback-on-failure path. Deletion is Phase C.
- **Retiring `VOICEPI_DICTATE_BACKEND=rust-session`.** That env var
  keeps working as a lower-level opt-in (in-process session sink
  alongside a Python worker) for anyone using it for diagnosis.
  Consolidating the two flags is a Phase-C-or-later cleanup.
- **Consolidating other `VOICEPI_*_BACKEND` env vars.** Same rationale
  as the reference doc's non-goal on this.
- **A PyO3 or C-ABI shared library.** Rejected in Option 2 above; not
  reconsidered here.
- **UI Rust-first refactor beyond what Phase B strictly needs.** The
  UI still reads `RuntimeEvent`s from the same channel; the source of
  the events changes from a Python child's streamed stderr to an
  in-process sender, but the UI code path is unchanged. Any egui
  refactor is out of scope.
- **Removing the `python -m whisper_dictate.runtime` entrypoint
  outright.** Even after Phase C, users may want to run the Python
  worker directly for debugging; the entrypoint stays until we have
  a Rust replacement for every one of its `--doctor`,
  `--list-audio-devices`, `--test-audio-device`,
  `--record-corpus-item` flags (see `worker_command.rs:132-262`).
