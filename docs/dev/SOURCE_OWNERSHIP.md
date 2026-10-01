# Source ownership

The shipped application and repository checks are native Rust. Rust source
lives in `src/rust`.

| Area | Primary modules |
|---|---|
| CLI and entrypoints | `cli.rs`, `main.rs`, `whisper-dictate-gui.rs` |
| Runtime lifecycle | `runtime/`, `hotkey/`, `dictate/session/` |
| Audio and DSP | `audio/`, `audio_dsp/` |
| Local and cloud STT | `whisper/`, `cloud_api/`, `dictate/backends/` |
| Formatting and dictionaries | `formatting.rs`, `postprocess/`, `dictionary/` |
| Text injection | `injection/`, `dictate/backends/inject.rs` |
| Desktop UI | `ui.rs`, `ui/` |
| History, metrics, diagnostics | `history.rs`, `telemetry.rs`, `diag.rs` |

The UI supervisor runs the session in-process. Reduced builds that omit a
required Cargo feature return an explicit error and do not silently select a
different runtime.

## Raw hotkey listener

The raw listener under `hotkey/manager/rdev_driver/` separates startup wiring,
readiness, native listener lifetime, callback dispatch, key conversion and
heartbeat policy. The callback records observed events before self-injection
filtering and only queues redacted diagnostics; liveness changes happen before
exit logging. Companion tests exercise synthetic callback events and scan the
actual native-listener module for readiness and liveness ordering guarantees.
There remains one native hook owner, not one listener per policy module.

## Windows hotkey driver

The Windows `RegisterHotKey` facade owns the native message thread and its
registration lifecycle. Chord parsing, modifier-family checks, registration
planning and transition policy live in focused modules under
`hotkey/manager/win_registerhotkey/`, each with companion tests. These policies
do not install listeners or change side-specific chord fallback to the raw
listener. The facade's native startup/shutdown test covers the ownership
boundary.

## Repository policy

`src/rust/tests/repository_policy.rs` and the other Rust policy tests enforce
the repository contracts across source, CI workflows, and packaging.
Keep those guards updated whenever a packaging or workflow boundary changes.

When changing production behavior, add the narrowest useful Rust regression
test and keep the implementation ownership in the module listed above.

## Diagnostic pipeline

`diag.rs` retains the public API, macros and shared callback queue state.
`diag/config.rs` owns cached level gates; `logger.rs` owns the single tee sink;
`panic.rs` owns the independent panic channel. `queue.rs` admits callback records
without blocking, `writer.rs` accounts for overload episodes, `startup.rs`
reports spawn failures, and `shutdown.rs` bounds sentinel admission and waiting.
The producer and drain use the same gate and ledger. Structural tests inspect
these implementation modules, not facade reexports; all sink and shutdown
regressions remain registered through `diag_tests.rs`.
