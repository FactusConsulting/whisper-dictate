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

## Injection dispatcher

`injection/dispatcher.rs` preserves the public facade. Its `engine` module owns
lazy native keyboard construction and platform dispatch; `chain` owns Linux
helper eligibility and the no-retry barrier after possible progress. `protocol`
owns the existing JSON envelopes and method selection, and `outcome` owns
result/partial-progress reporting. Clipboard ownership stays in `paste` and
the runtime injection backend; moving these policies does not add a fallback
or repeat a partially successful injection. Companion tests cover each owner
and exercise the facade with the existing serialized shapes.

## Repository policy

`src/rust/tests/repository_policy.rs` and the other Rust policy tests enforce
the repository contracts across source, CI workflows, and packaging.
Keep those guards updated whenever a packaging or workflow boundary changes.

When changing production behavior, add the narrowest useful Rust regression
test and keep the implementation ownership in the module listed above.
