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
