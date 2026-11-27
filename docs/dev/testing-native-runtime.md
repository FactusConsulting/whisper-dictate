# Native runtime verification

All supported builds use the Rust runtime. Validation requires the Rust
toolchain and the platform tools listed below.

## Automated checks

Run the Linux developer gate from the checkout:

```powershell
pwsh scripts/dev/dev-check.ps1
```

The gate lints with `ui-egui-glow`, runs one shipping `cargo nextest` suite
(including repository policy), and runs CLI smokes against the `wd` built by
nextest.
Nextest does not run doctests, so the gate runs shipping doctests separately.
It does not build a separate optimized binary.

CI runs one full shipping test suite per operating system. Windows runs
nextest directly, including the native GUI smoke and its exclusive nextest
resource reservation. Linux runs nextest through `cargo llvm-cov` to produce
the coverage report. The shipping cells reuse those built `wd` binaries for
`--help`, `--version`, and `config path`. Linux shipping runs rustfmt once;
Windows shipping also runs the release-script fixture. The required `smoke`
contexts report the result of the Rust feature matrix without compiling
anything separately. The required `unit` job owns repository-policy tests
and doctests.

The `clippy-glow` cell lints on both operating systems; the Linux invocation
also produces the lint report. On pull requests, the `features` cell checks
all targets with default features and with each supported partial feature set:
`rust-hotkeys`, `audio-capture`, `rust-hotkeys,rust-injection`, and
`whisper-rs-local,audio-capture,rust-injection`. It runs focused default-only
regressions on both operating systems and a partial-set regression on Linux.
These checks exercise feature gates without building a second full test suite.

Optimized builds cover different risks. The Windows release build validates
release CLI output and injection idempotency on Rust-relevant pull requests
as well as on push and release validation. The Linux release build in
`rust-release` checks linking on push to `main` and during release validation.
The Ubuntu 26.04 compatibility job builds the locked, optimized shipping
CLI once and runs `models list` and the push-to-talk, injection, audio
capture, and tiny.en model-load checks directly against that executable.
Packaged release validation remains separate.

On pull requests the Linux shipping cell also validates the 500 ms
audio-capture self-test report using its coverage-built CLI. The Ubuntu
26.04 compatibility job uses the same checker. Only an explicit
missing-input-device report is a skip; panics, malformed reports, and
opened streams that deliver no samples fail the check. Both environments
declare `jq`; report-checker fixture tests run in the shipping suite.

To check the locked Rust dependency graph against RustSec advisories, install
`cargo-audit` and run:

```powershell
cargo audit --file src/rust/Cargo.lock
```

The reusable `cargo-audit` workflow runs once for dependency-relevant pull
requests and `main` pushes, release validation, and every Monday. The tests
workflow routes PR, push, and release calls through the required `unit` context;
the scheduled run invokes the same implementation directly. Address findings
by updating the dependency; do not suppress an
advisory without documenting the reason and expiry in `.cargo/audit.toml`.
Current exceptions are tracked in the same file and must be removed by their
review date.

## Windows microphone release check (per-recording capture)

The shipping WASAPI path opens the capture device when a recording starts and
closes it when the recording ends, instead of holding one stream for the
runtime's lifetime. CI runners have no audio device, and the lifecycle tests
use a fake opener, so the handover to the real WASAPI stack is verified by
hand on Windows whenever the capture lifecycle changes:

1. Start the app and let it reach Running. Before pressing anything, confirm
   the Windows microphone indicator (taskbar, and Settings > Privacy &
   security > Microphone > "Recently accessed") shows **no** app using the
   microphone.
2. Hold push-to-talk (or press once in toggle mode) and speak. The indicator
   turns on and names whisper-dictate.
3. Release (or press again in toggle mode). Within the release tail the
   indicator turns off. While idle, open another recorder (Voice Recorder,
   or Sound settings > Test your microphone) and confirm it can capture:
   whisper-dictate must not hold the device.
4. Press again and confirm the transcript still arrives, proving the runtime
   reopens the device after having released it. Repeat once with a Bluetooth
   headset if one is available: the first ~1 s after the press may be lost to
   the profile switch, which is expected and logged as a slow open.

The diagnostic log records each transition: `[audio/capture] opening ...`,
`[rust-session-audio] capture opened in N ms`, and `capture closed`. An idle
runtime must log no `opening` line at all.

## Linux Secret Service overwrite check

The normal CI runners have D-Bus client libraries but no logged-in, unlocked
Secret Service collection. A passing fallback-file test there would not prove
that an existing keyring item can be updated. The real integration check is
therefore opt-in and deliberately fails unless it reaches Secret Service.

Run it from a logged-in Linux desktop session whenever the Linux keyring stack
or its locked dependencies change:

```bash
unset VOICEPI_DISABLE_OS_KEYRING
cargo test --manifest-path src/rust/Cargo.toml --locked \
  -p whisper-dictate-app \
  ui::api_key_store_tests::linux_secret_service_overwrites_existing_api_key \
  -- --ignored --exact --nocapture
```

The test uses a unique temporary credential identity, writes an initial API
key, overwrites it, reads the replacement directly from Secret Service, and
checks the fallback copy. It also overwrites and reads non-UTF-8 data through
the same keyring entry, exercising the stricter binary-secret path used by KDE
Secret Service implementations. The temporary credential is removed on exit.
Record the desktop/session, date, and final `test result` line in the dependency
PR. A fallback-only result or a missing/locked `org.freedesktop.secrets`
service is a failed or blocked verification, not a pass.

The `cargo-outdated` workflow publishes a scheduled Monday report of outdated
root dependencies. Review its output before updating FFI or system crates; it
does not change dependencies automatically.

## Native CLI smoke checks

Build the CLI with the feature set needed by the command under test, then
exercise the native verbs directly:

```powershell
cargo run --manifest-path src/rust/Cargo.toml --features whisper-rs-vulkan -- transcribe-file .\recording.wav
cargo run --manifest-path src/rust/Cargo.toml -- doctor
cargo run --manifest-path src/rust/Cargo.toml -- setup --help
```

The Windows hardware-dependent checks are documented in
[`scripts/manual-test/README.md`](../../scripts/manual-test/README.md). The
Wayland end-to-end procedure is in
[`wayland-user-smoke.md`](wayland-user-smoke.md).

## GPU-only local transcription checks

For the `transcribe-file` smoke check, first configure local Whisper, select
Vulkan, and install a GGML model that fits the test GPU. The command tests
inference, so it must run on a machine with a supported GPU. CPU-only CI can
still run `wd self-test whisper-load` to verify that a GGML file loads. That
diagnostic does not transcribe audio and is not evidence that local
transcription is available.

For manual coverage of local STT:

1. On a Vulkan-capable machine, confirm a local Whisper model that fits loads
   and transcribes. For Edda, follow the
   [manual GGML setup](../STT_BACKENDS.md#use-an-edda-model-from-a-local-file)
   and record a Danish utterance.
2. On a machine with no supported GPU, verify that local inference cannot
   start or reports a GPU requirement when used. Repeat with an existing
   `device: "cpu"` setting and verify the app reports that CPU is unsupported
   for local transcription.
3. On a machine with enough total GPU memory for the model but not enough
   free memory, verify the runtime reports a capacity error rather than
   using CPU. Free memory or choose a smaller supported model, then retry.
4. Confirm a hosted provider is recommended without changing the selected
   route or uploading audio. Verify it remains available for explicit
   configuration and that `local_only` still blocks remote STT routes.

## Physical hotkey focus matrix

Stop normal dictation. On Windows and X11, open **Speech > Test shortcut in
both windows** and record the actual installed driver plus the two visible
results. On Wayland, confirm that the control is disabled and both focus
results are documented as unavailable. The diagnostic must not open the
microphone, load Whisper, or inject text.

| Platform/session | Chord | Expected driver | Another window focused | WhisperDictate focused |
|---|---|---|---|---|
| Windows | `pause` | `win_registerhotkey` | press + release verified | press + release verified |
| Windows | `ctrl+f9` | `win_registerhotkey` | record observed result | record observed result |
| Windows fallback | `ctrl_l+f9` | `rdev` with focus-risk warning | record observed result | record observed result; use `pause` if it fails |
| Linux X11 | `pause` | `rdev` | press + release verified | press + release verified |
| Linux Wayland | `pause` | `evdev` | unavailable - no portable focus-owner API | unavailable - no portable focus-owner API |

For each row, confirm Settings distinguishes preflight from the installed
driver and keeps the two focus results separate. Editing the chord must make a
prior result stale, and restarting the app must clear the diagnostic result.
If either supported focus context fails, confirm the UI recommends `pause` or
another tested chord rather than changing configuration automatically. On
Wayland, record the installed `evdev` driver from normal runtime status but
treat the disabled focus-classification workflow as unavailable by design, not
as an `evdev` regression. Listener install or chord-event failures on Wayland
still belong to the `evdev`
access/device-permission path; X11 and Windows fallback failures belong to the
`rdev` listener path.

## Diagnostics

Set `VOICEPI_LOG=debug` for lifecycle and configuration decisions. Set
`VOICEPI_LOG=trace` for hotkey, audio, session, and injection breadcrumbs.
On Windows, the GUI writes the diagnostic stream to
`%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log`.
The guided shortcut test logs only its selected driver, registration outcome,
configured-chord press/release/cancel signals, and focused/unfocused result. It
does not log unrelated keys, even when the parent has
`VOICEPI_HOTKEY_DEBUG=1`. Each chord signal carries the foreground-process
classification captured by the child action sink at the event source; pipe or
GUI-frame latency never reclassifies it later. Pure Wayland reports the focus
classification as unavailable because no portable owner API exists. Stopping
the test must terminate and reap the child process so repeated rdev/evdev checks
do not leave process-lifetime listener threads in the GUI.
