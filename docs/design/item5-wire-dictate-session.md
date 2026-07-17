<!--
Design doc for audit item 5 (see docs/architecture-audit-2026-07-16.md § "5.
Wire dictate::DictateSession into the shipping PTT loop"). This is a
planning artefact — no code, no timeline commitments beyond the estimates
at the end. Author: item-5 planning subagent, 2026-07-17.
-->

# Item 5: Wire Rust `DictateSession` into the shipping PTT loop

## Context

**What this delivers.** The full per-utterance state machine, real
Whisper/Enigo backends, and coordinator wiring for a Rust-native dictation
loop already live under `src/rust/dictate/` (session, backends,
audio_route). All of it has unit tests; none of it is called from the
shipping PTT hot path. Item 5 is the plan for flipping that switch —
carefully, behind an opt-out flag, on a phased rollout that lets us walk
back without another baseline reset.

**Why the previous attempt (v1.20.x) failed.** Commit `cfbbfa4` — "Wave 8
Part 2: delete Python plumbing, require worker-rust delegation" — landed
as v1.20.0-rc.1 and shipped as v1.20.0. It went straight to
`RuntimeSupervisor::start` unconditionally swapping the child command to
`whisper-dictate worker-rust` with no Python fallback. The regression
tail:

| Version | Commit | Regression |
|---|---|---|
| v1.20.0 | `cfbbfa4` | Deleted Python worker + no fallback path |
| v1.20.0 | `2a84e8a` | Onboarding wizard stuck (fixed in #461) |
| v1.20.1 | `9cb6070` | Wayland PTT missing evdev listener (#462) |
| v1.20.2 | `6042502` | PipeWire RT crash-loop + self-injection PTT wedge on Linux (#467) |
| v1.20.6 | `d2b7a9a` | `PIPEWIRE_QUANTUM` misconfigured DMIC (#474) |
| v1.20.7 | `a6cc5c9` | Windows self-injected keys wedged PTT after first use (#476) |
| v1.20.8 | `ea04c59` | Debug-only trace itself broke Windows PTT — reverted `8360b78` |
| v1.21.0 | `333a2b4` | Wholesale reset to v1.19.0-rc.3 baseline (#482) |

The single-line summary: **rdev on Windows was echoing our own
`SendInput` keystrokes back into the PTT tracker**, wedging the second
press until the app restarted; on Linux the ydotoold virtual `/dev/input`
node did the same to the evdev listener. Both needed independent fixes
(`inject_guard::InjectionGuard` on Windows, device-enumeration exclusion
on Linux). The Wayland listener was missing altogether. The reset
discarded all of those fixes — none of `hotkey/inject_guard*` survives in
main today (`grep -rn inject_guard src/rust/hotkey/` → empty), so the
same self-feedback bug would re-appear immediately if we shipped as-is.

**Why we now think we can do it safely.** The audit's item-5 plan builds
on four things the v1.20 attempt didn't have:

1. **A flag with a real fallback.** v1.20.0 deleted Python — there was
   nowhere to fall back TO. This plan keeps Python as the default until
   Phase B ships, and preserves it as an emergency fallback beyond that.
2. **Named prerequisites.** The self-injection guard, the Wayland evdev
   listener, and PipeWire-quantum handling are called out below as
   pre-Phase-A blockers, not "we'll notice at RC time".
3. **A phased rollout with a bake requirement.** No 30-day telemetry, no
   Phase-C retire. v1.20 went from "off" to "only path" in one commit.
4. **A regression harness.** Item 5 is gated on integration tests that
   exercise `press → record → transcribe → inject → release` five times
   in a row on each supported platform — the exact regression class
   v1.20.7 shipped in.

## Current state (v1.21.2 baseline)

### Shipping PTT loop (what runs today when a user presses the hotkey)

1. **Hotkey listener** — the Python entrypoint
   `src/python/whisper_dictate/runtime.py:754` instantiates
   `Dictate(...).run()` on the main thread. `Dictate` mixes in
   `KeyBackendMixin` from `src/python/whisper_dictate/vp_keys.py`.
2. **Key press dispatch** — `KeyBackendMixin.on_press`
   (`src/python/whisper_dictate/vp_keys.py:164-198`) matches the chord
   and calls `self._owner._start()` on the pynput listener thread.
3. **Capture start** — `Dictate._start`
   (`src/python/whisper_dictate/vp_dictate.py:528-590`) reloads live
   config, captures the target window, bumps `_record_epoch`
   (`vp_dictate.py:543`), emits `status=opening`, then dispatches to
   `_start_rust_stdin`, `_start_arecord`, or `_start_sounddevice`
   depending on the audio source; emits `status=recording`.
4. **Frame accumulation** — `CaptureMixin` in
   `src/python/whisper_dictate/vp_capture.py` streams PCM into
   `self.frames` while `self.recording` is true.
5. **Key release dispatch** — `KeyBackendMixin.on_release`
   (`src/python/whisper_dictate/vp_keys.py:215-248`) spawns
   `_stop_and_transcribe` on a background thread.
6. **Stop + transcribe + inject** —
   `Dictate._stop_and_transcribe`
   (`src/python/whisper_dictate/vp_dictate.py:683-770+`) waits
   `release_tail_ms`, closes streams, calls `self._transcribe_pcm(pcm)`
   (delegates to `_transcribe_detail` from `vp_transcribe.py`), then
   `InjectMixin` types the transcript into the active window.
7. **Chord cancel** — `KeyBackendMixin` may spawn
   `Dictate._cancel_and_discard(epoch)`
   (`src/python/whisper_dictate/vp_dictate.py:662-681`), which uses the
   epoch to guard against stale cancels (the race documented at
   `vp_dictate.py:140-147`).

The Rust supervisor (`src/rust/runtime.rs`) is only the launcher: it
builds a `WorkerCommand` (`runtime.rs:1275-1311`) that runs
`python -m whisper_dictate.runtime`; the whole PTT loop lives in the
Python child process.

### Rust `DictateSession` (what a hypothetical invocation looks like)

The Rust equivalent lives under `src/rust/dictate/`:

- `session/mod.rs:85-445` — `DictateSession<T: TranscribeBackend,
  I: InjectBackend>` with `state()`, `start()`, `push_frame()`,
  `stop_and_transcribe()`, `cancel()`. Byte-for-byte mirror of the
  Python state machine including the chord-race epoch guard
  (`mod.rs:399-423`).
- `session/types.rs` — `SessionState`, `SessionConfig`,
  `TranscribeBackend` / `InjectBackend` traits.
- `session/wire.rs` — status/utterance event emitter.
- `backends/whisper_local.rs` — production `TranscribeBackend` behind
  `--features whisper-rs-local`.
- `backends/inject.rs` — `EnigoInjectBackend` behind
  `--features rust-injection`.
- `audio_route/mod.rs` — bridges `AudioPipeline` events into the
  session; behind `--features audio-in-rust`.
- Coordinator wiring at `src/rust/hotkey/coordinator/` produces
  `Stage` events that fan out to session `start`/`stop`/`cancel`.

Unit coverage is thorough: `session/tests_ported.rs` ports the six
characterisation tests from `src/python/tests/test_dictate_loop.py`;
`session/tests_transitions.rs`, `backends/inject_*_tests.rs`,
`backends/whisper_local_tests.rs`, and
`audio_route_*_tests.rs` (four files) cover the transitions, backend
error paths, and audio-route bridge invariants.

**The gap is not "does Rust work?" — it's "who calls it in production?"**
Nothing in the shipping binary constructs a `DictateSession` today.

## The wire-up point

**Today's dispatch decision:** `src/python/whisper_dictate/runtime.py:754`
constructs and runs the Python `Dictate` unconditionally (there is no
env-var branch — the reset removed the `worker-rust` swap in
`RuntimeSupervisor::start` from `cfbbfa4`; see
`src/rust/runtime.rs:1271-1311` today).

**The swap** must happen at one of two levels:

1. **Rust supervisor** (`RuntimeSupervisor::start` in
   `src/rust/runtime.rs`): branch on `VOICEPI_DICTATE_ENGINE`. When set
   to `rust`, build and run a `worker-rust`-equivalent in-process
   session driver instead of spawning `python -m
   whisper_dictate.runtime`. This is the shape v1.20 took — good on
   perf, no IPC — but it requires a full Rust dispatch loop
   (hotkey listener → coordinator → session → real backends → tray/UI
   status events) that today has no single entry point in main.
2. **Python entrypoint** (`runtime.py::_run_session`): branch on
   `VOICEPI_DICTATE_ENGINE`. When set to `rust`, replace the
   `Dictate(...).run()` call with `subprocess.run([whisper_dictate,
   "dictate-run", ...])`. Adds one process boundary but preserves the
   supervisor unchanged and gives us a natural CI-driveable verb.

**Recommendation: option 2 for Phase A**, option 1 for Phase B.
Rationale: option 2 lets us ship the flag with no supervisor churn and
no cross-thread coordination changes — the risk surface is contained
to a single `Popen`. Option 1 is the eventual target because it drops
the extra process, but it should follow only once the CLI-verb form
has flown enough hours to build confidence.

Either level needs a new public verb: **`whisper-dictate dictate-run`**
(name TBD; could be `worker` to echo v1.20). It's a thin CLI wrapper
around the session-plus-coordinator plumbing. Not a hidden RPC — this
is what CI drives and what Phase A's subprocess call targets.

## Proposed approach: phased with opt-out flag

### Phase A: dual-path infrastructure (safe)

- **Env var:** `VOICEPI_DICTATE_ENGINE={python,rust}`.
- **Default:** `python` — unchanged behaviour for every user who does
  not opt in.
- **CLI verb:** add `whisper-dictate dictate-run` as a public
  subcommand. Wraps `RustDictationLoop::run()` (a new module gluing
  the hotkey coordinator, `DictateSession`, real backends, and
  `AudioRoute` behind the four required cargo features).
- **Dispatch:** in `runtime.py::_run_session`
  (`src/python/whisper_dictate/runtime.py:722-764`), branch on the env
  var before constructing `Dictate`; when `rust`, exec the new verb
  and forward stdio + exit code.
- **Ships as:** v1.22.0-rc.1 (prerelease channels — choco
  `--prerelease`, nix RC tag; brew/winget skipped until final).
- **Regression coverage that MUST pass in CI before this ships:**
  - Windows: hotkey capture, press-record-inject 5x, chord-cancel,
    settings tab renders (release-matrix smoke).
  - Linux X11 + Wayland: same, with evdev listener path exercised.
  - macOS: same (if supported by the release matrix; today PTT works
    but accessibility permission is manual).
  - `pytest src/python/tests`: full green with `VOICEPI_DICTATE_ENGINE`
    unset (proves default path untouched).
  - `pytest src/python/tests` again with
    `VOICEPI_DICTATE_ENGINE=rust`: subset that exercises the dispatch
    (tests that stub the `Popen` call can pin the flag semantics).
  - `cargo test --features
    "audio-in-rust,whisper-rs-local,rust-injection,rust-hotkeys"`:
    green — this is the CI leg the audit's "recommended container test
    setup" calls out.

### Phase B: Rust default with Python fallback (guarded)

- **Default flips to `rust`.**
- **Ships as:** v1.22.0 (final channel after two clean weeks of
  Phase A prerelease telemetry).
- **Fallback path:** if the Rust session driver panics OR exits
  non-zero within the first N seconds (N=10, tunable via
  `VOICEPI_DICTATE_ENGINE_FALLBACK_WINDOW_S`), the supervisor logs a
  `runtime.event=fallback,engine=rust,reason=<...>` line, then
  respawns with `VOICEPI_DICTATE_ENGINE=python` sticky for the
  process lifetime.
- **Release notes MUST include:**
  `If PTT wedges or hangs, set VOICEPI_DICTATE_ENGINE=python and file
  an issue with the fallback log line.`
- **Regression coverage:**
  - Everything in Phase A, PLUS
  - Chaos test: a mocked backend panic on the first utterance verifies
    the fallback fires and the second utterance goes through Python.
  - Long-transcription test: >8 s of speech does not wedge the loop
    (the v1.20.7 regression was a 5-second failure window; use a
    larger margin).
  - Idempotency test: 5 back-to-back PTT presses on Windows across
    processes with the injection guard active. This is the regression
    that broke v1.20.7.

### Phase C: retire Python worker (aggressive — only after evidence)

- **Precondition:** 30+ calendar days of Phase B in production with
  zero fallback events surfaced by the telemetry sink. If ANY
  fallback events fire, extend the window; do not proceed on cadence.
- **Ships as:** v1.23.0.
- **Scope:** delete `src/python/whisper_dictate/vp_dictate.py`,
  `vp_capture.py`, `vp_keys.py`, `vp_keys_solo.py`,
  `vp_keys_capture.py`, `vp_transcribe.py`, `vp_inject.py`, and the
  `_run_session -> Dictate(...).run()` path in `runtime.py`. Retire
  `VOICEPI_DICTATE_ENGINE` (Rust becomes unconditional).
- **Regression coverage:** everything in Phase B, plus a "cold
  install" smoke on all three platforms verifying the removed Python
  is not referenced by any packaging script or docs.

## Test plan

Per phase, the following MUST be green in CI before promotion (all
platforms in the release matrix; XFAIL is not a pass):

**Common baseline (every phase):**

- `cargo test --features
  "audio-in-rust,whisper-rs-local,rust-injection,rust-hotkeys"` on the
  `.devcontainer` (Ubuntu 26.04).
- `pytest src/python/tests src/tests/python` — full suite green with
  the phase's default value of `VOICEPI_DICTATE_ENGINE`.
- Every public CLI verb (`whisper-dictate {run, ui, settings,
  dictionary, models, config, history, devices, inject-text,
  dictate-run}`) exits 0 on a smoke invocation.
- `whisper-dictate devices` enumerates the PulseAudio null sink in
  the container.

**Regression harness (must be built for Phase A):**

- Windows GHA runner: script drives 5 back-to-back PTT presses via
  `SendInput` against a bench app; asserts the fifth press still
  produces an utterance event. This is the v1.20.7 regression test —
  it did not exist and would have caught the ship-blocker.
- Linux Wayland container (sway + xvfb-like harness): same 5-press
  script with `WAYLAND_DISPLAY` set; verifies the evdev listener path.
- Linux X11: same 5-press script with only `DISPLAY` set.
- Long-utterance test (>8 s recorded WAV replayed through the
  simulator): asserts single utterance event, no wedge.
- Chord-cancel test: press PTT, hold, add second key, verify cancel
  event and next press works.

**Cannot test in CI (documented, must be manual before RC -> final):**

- Real keystroke injection into a real user-focused window on
  Windows/Wayland/macOS.
- Real GPU whisper.cpp path (Vulkan/CUDA).
- macOS accessibility permission dialog acceptance.

## Rollback plan

- **Phase A -> v1.22.0-rc.1:** users can revert by unsetting the flag
  (`VOICEPI_DICTATE_ENGINE`); default is Python. Zero blast radius —
  worst case a user opts in and hits a bug, they unset and continue.
- **Phase B -> v1.22.0:** users on the broken path can
  `VOICEPI_DICTATE_ENGINE=python` and continue. The auto-fallback
  should catch most cases before the user notices, but the escape
  hatch is documented in release notes.
- **v1.22.1 as hotfix path:** if Phase B fails widely, ship a
  point-release that flips the default back to `python` (one-line
  change, no code migration). Reserve the tag proactively.
- **Wholesale reset (as v1.20.x -> v1.21.0):** do NOT do this again if
  avoidable — it discarded 8 releases of unrelated fixes (Vulkan,
  PipeWire, Wayland evdev, self-injection guard). If we must revert,
  revert only the item-5 commits, not the branch.

## Risks (ranked)

1. **rdev self-injection feedback loop on Windows.** rdev 0.5's
   `WH_KEYBOARD_LL` callback does not inspect `LLKHF_INJECTED`, so
   every `SendInput` from `EnigoInjectBackend` echoes back into the
   PTT tracker and wedges the next press. Fixed in v1.20.7 by
   `hotkey/inject_guard::InjectionGuard` — **that code did not survive
   the reset to v1.19.0-rc.3**; `grep -rn inject_guard
   src/rust/hotkey/` is empty in main today. **Mitigation:**
   re-land the injection guard as a hard prerequisite before Phase A
   (see Prerequisites). **Must resolve by:** Phase A.

2. **Wayland PTT with only rdev + no evdev listener.** rdev's Linux
   backend uses X11; on pure Wayland it misses keys unless an evdev
   listener is added. Fixed in v1.20.1 (#462) on the Python side; the
   Rust `hotkey/manager/rdev_driver.rs` has no evdev fallback today
   (`grep -n evdev src/rust/hotkey/manager/*.rs` -> nothing).
   **Mitigation:** add an evdev listener path to the Rust hotkey
   manager, mirroring `vp_keys.py`'s Linux code. **Must resolve by:**
   Phase A (Linux users are a significant slice).

3. **Text-injection self-feedback on Linux (ydotool).** Same
   root cause as (1) but on Wayland: the ydotoold virtual
   `/dev/input` node feeds back into the evdev listener. Fixed at
   device-enumeration level in v1.20.2 (#467). **Mitigation:** port
   the exclusion into the new evdev listener from risk (2). **Must
   resolve by:** Phase A.

4. **PipeWire quantum + RT-scheduler crash-loop.** v1.20.2/v1.20.6
   traced a startup crash to `PIPEWIRE_QUANTUM=4096` and RT-scheduler
   assumptions. cpal defaults may repeat this on user PipeWire
   installs. **Mitigation:** replicate the settings from #467/#474 in
   the Rust cpal open-config path; add a PipeWire-quantum-mismatch
   smoke check to CI's Linux leg. **Must resolve by:** Phase B (Phase
   A shields us because the default is Python).

5. **Whisper model cold-load latency and OOM.** Loading a `medium.en`
   GGML on first press can take 3-8 s on CPU; on constrained VRAM
   the load fails. Python's `_load_model` shows a spinner and defers.
   Rust `WhisperLocalTranscribeBackend` needs the same UX or the user
   sees a silent PTT press. **Mitigation:** preload at supervisor
   start (already the pattern for whisper-rs-local); surface a
   `status=loading` event; ensure fallback to Python catches OOM
   panics. **Must resolve by:** Phase B.

6. **Audio-device permission + open-config edge cases.** Windows
   WASAPI rejects certain sample-rate/channel combos; cpal's config
   matrix may not mirror Python's sounddevice negotiation. The
   Python path swallows this via `_handle_capture_start_failure`
   (`vp_dictate.py:592-645`). **Mitigation:** port the same recovery
   into `RustDictationLoop`; add a `whisper-dictate devices test
   NAME` verb (audit item 3) so failures surface at diagnose-time,
   not press-time. **Must resolve by:** Phase B.

## Prerequisites (must land BEFORE Phase A)

**Blocking prerequisites for Phase A:**

- **PR #494 — `runtime.rs` split** (audit item 1). Splits the 2224-LOC
  `runtime.rs` into `runtime/{supervisor, audio_bridge, hotkey_install,
  worker_command}.rs`. Item 5 will need to add a new dispatch branch
  in the supervisor; landing it into a modular tree makes the diff
  reviewable. Status per audit: "already armed".
- **Re-land the injection guard** (`hotkey/inject_guard/`). Lifted
  wholesale from PR #476's diff (still in git history at commit
  `a6cc5c9`). Adds `Arc<OnceLock<InjectionGuard>>` armed by
  `EnigoInjectBackend` and consulted by the rdev driver. Test:
  `windows_5_press_smoke.rs` (the regression harness above) must be
  green.
- **Add Rust-side evdev listener** for Linux Wayland. Mirror
  `vp_keys.py`'s device-open matrix; exclude ydotoold's virtual node
  by the same mechanism #467 used. Test:
  `wayland_5_press_smoke.rs` (regression harness) must be green.
- **Item 2 chunk H (or later) —
  `whisper-dictate hotkey listen` verb.** Landing a CLI that just
  runs the hotkey coordinator lets item 5's regression harness drive
  the listener without spawning a full worker. Recent audit item 2
  chunk F (`hotkey capture`) is already merged (commit `59cd41f`);
  the `listen` verb is a small extension.

**Recommended but not blocking:**

- Audit item 4 (retire Python dictionary parity). Reduces the Python
  surface area we still ship, so Phase C is smaller.
- Native `whisper-dictate doctor` + `bench` verbs (Wave 8 Part 3
  follow-ups called out in `cfbbfa4`'s commit body) — reduces the
  set of Python paths still exercised at install time.

## Estimate

Rough calendar-day estimates including bake time. Engineering-day
figures assume one senior engineer full-time with normal review
cycles.

| Phase | Eng days | Calendar days (incl. bake) |
|---|---|---|
| Prereqs (runtime split + injection guard + evdev listener + `hotkey listen` verb) | 5-7 | 7-10 |
| Phase A (dual-path + `dictate-run` verb + regression harness + RC bake) | 4-6 | 10-14 (2-week prerelease bake) |
| Phase B (default flip + fallback + chaos tests + point-release safety net) | 3-4 | 14-21 (2-3 week prod bake) |
| Phase C (retire Python) | 3-5 | 30+ (mandatory 30-day zero-fallback bake) |
| **Total** | **15-22** | **~60-75 days end-to-end** |

The calendar-day slack is deliberate. v1.20.x compressed a similar
scope into ~2 weeks and shipped four regressions. Bake time is where
this plan differs — the 30-day zero-fallback gate before Phase C is
the load-bearing item.

## Non-goals

Explicitly out of scope for item 5:

- **UI Rust-first refactor.** egui already renders in Rust; no
  changes to `ui/*` beyond the tray-state feedback the new engine
  needs to emit. The Settings tab keeps talking to the supervisor
  via the same events.
- **New user-facing features during the swap.** No new hotkey modes,
  no new injection backends, no new STT vendors. The point is a
  clean cut-over; feature work waits for v1.24+.
- **Cloud STT changes.** Cloud transcription already runs through
  Rust (`cloud_api/transcribe.rs`); the swap is local-Whisper +
  injection + capture + hotkey, not cloud path.
- **SQLite history migration.** Per audit appendix, history is JSONL
  today; migrating to SQLite is a separate proposal.
- **macOS accessibility auto-grant.** Still manual; item 5 will not
  attempt to script the permission dialog.
- **Removing `VOICEPI_TRANSCRIBE_BACKEND`, `VOICEPI_INJECTION_BACKEND`,
  `VOICEPI_DEVICES_BACKEND`, `VOICEPI_HOTKEY_BACKEND`.** Those env
  vars gate individual sub-features; item 5 introduces a
  new `VOICEPI_DICTATE_ENGINE` at the loop level. Consolidating the
  others is a Phase-C-or-later cleanup.
