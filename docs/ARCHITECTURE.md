# whisper-dictate architecture

## Product boundary

whisper-dictate is one native Rust application with two user entry points:

| Surface | Responsibility |
|---|---|
| `wd` CLI | Terminal dictation, configuration, diagnostics, model management, calibration, transcription, history, and text injection. |
| `wd-gui` desktop app | Settings, runtime lifecycle, tray integration, compact status, model downloads, and live diagnostics. |

Both entry points use the same modules under `src/rust`. Shipping builds contain
the complete runtime; reduced developer builds report missing features instead
of selecting a different implementation.

## Runtime ownership

The desktop app owns one managed in-process runtime. Start, stop, and restart
are explicit state transitions shown in the UI. The compact status surface and
tray are views and controls over that runtime; they do not create additional
instances.

`wd run` owns the same runtime components for the lifetime of its terminal
session. A cross-process lock allows only one active push-to-talk listener per
user when its per-user lock can be acquired. If its directory or file cannot
be created or opened, or the filesystem cannot provide the advisory lock, the
runtime logs a warning and deliberately starts unguarded, so multiple listeners
remain possible in that degraded state.
Stopping the desktop runtime releases a held lock so a terminal session can
take ownership; starting the desktop runtime can be refused while another
process owns it.

## Dictation flow

```text
hotkey press
    -> capture audio
hotkey release
    -> finish the recording boundary
    -> validate and normalize audio
    -> transcribe locally or through a configured cloud endpoint
    -> apply dictionary replacements and optional post-processing
    -> inject or print the text
```

The controller serializes lifecycle actions so a session cannot record,
transcribe, and restart concurrently. Errors are surfaced to the UI or terminal
and leave the runtime in an explicit stopped or error state.

## Settings and credentials

The desktop app and CLI read the same JSON configuration:

- Windows: `%APPDATA%\WhisperDictate\config.json`
- Linux: `${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/config.json`

CLI flags override saved settings for one run. Environment variables provide a
headless compatibility surface. Settings marked live are applied at recording
boundaries; model, backend, device, and hotkey changes require an explicit
runtime restart.

The desktop app uses the operating-system credential store as its primary API
key store. It falls back to `api-keys.json` in the platform configuration
directory when the keyring is disabled, unavailable, or fails verification;
non-Windows platforms also retain that file after a successful keyring write.
Headless sessions can supply the documented API-key environment variables.
Credential values are excluded from normal logs and configuration exports.

## Audio capture and preprocessing

Native capture produces mono 16 kHz frames for the dictation session. The
platform device layer resolves the configured microphone, and the shared DSP
layer applies duration, level, signal-to-noise, gain, and trailing-silence
checks before transcription.

| Platform | Capture contract |
|---|---|
| Windows | Native WASAPI/CPAL capture and Windows device enumeration. |
| Linux | CPAL capture with PipeWire-compatible device selection. |

Calibration commands use the same decoding and signal-quality calculations as
normal dictation, so their recommendations match the runtime gate.

## Speech recognition

The supported engines are:

- `whisper`: local whisper.cpp inference using GGML model files;
- `openai`: an OpenAI-compatible transcription request using provider
  configuration for OpenAI, Groq, or a custom endpoint;
- `openai` + provider `nemotron` + `inproc://nemotron`: local Nemotron 3.5
  GGUF inference through the dynamically loaded NeMo-Speech.cpp C ABI. Official
  model/runtime assets are downloaded and SHA-256 verified into the per-user
  cache on first use; explicit paths remain available for offline installs.

Catalog-managed local models are downloaded only after an explicit user action
and are verified with SHA-256 before use. A model supplied through
`VOICEPI_WHISPER_MODEL_PATH` is checked for a supported file shape but is not
hash-verified, so its integrity remains the user's responsibility. The selected
model is loaded lazily and can be unloaded after the configured idle period.
Shipping builds include the local backend; Windows shipping artifacts also
include Vulkan acceleration.

Cloud credentials are resolved for the configured endpoint, and endpoint
provenance is checked before a stored key is sent. Local-only mode blocks
external speech and post-processing endpoints.

See [STT_BACKENDS.md](STT_BACKENDS.md) for the user-facing backend contract.

## Dictionary and post-processing

Dictionary terms are fitted into bounded term and character budgets before
being added to the transcription prompt. Replacements are applied separately
to the returned text. This keeps prompt size bounded while allowing exact
product-name and vocabulary corrections.

Optional post-processing runs after transcription and dictionary replacements.
It can use local Ollama or a configured OpenAI-compatible chat endpoint. The
result then follows the normal history and injection path.

## Text injection

| Platform | Injection contract |
|---|---|
| Windows | Native typing and clipboard paste with focused-window metadata. |
| Linux X11 | Native X11 typing/paste helpers and target-window restoration. |
| Linux Wayland | `ydotool`, `dotool`, or `wtype`, subject to compositor and input-device permissions. |

The runtime captures target metadata when recording begins. Windows and X11
can use an opaque target identifier to inject the text again. Wayland does not
provide a portable target-window identifier, so actions that require restoring
an old target remain unavailable there.

## Hotkeys

Windows selects among native hotkey drivers according to the requested chord.
Linux X11 uses the native global listener. Linux Wayland uses evdev and requires
read access to keyboard input devices, normally through the `input` group.

The listener emits press and release events into the shared coordinator. The
coordinator owns recording boundaries and ignores duplicate or invalid
transitions. `wd doctor` reports platform permissions and helper readiness;
`wd hotkey` provides bounded listener diagnostics.

## UI and observability

The desktop runtime publishes structured state to the main and compact UI:
starting, listening, recording, transcribing, injecting, stopped, or error.
Transcript cards, copy/reinject actions, microphone health, and progress are
derived from that state and retained within bounded session history.

`wd run --json` and `wd dictate-run --json-events` expose non-utterance worker
events as
`{"kind":"worker","event":"status","state":"...","payload":{...}}`.
Alongside the normal pipeline states, microphone recovery uses these stable
states:

- `audio-fallback`: the system-default microphone passed bounded health
  validation after the configured microphone could not be opened;
- `audio-recovered`: a microphone opened by background recovery passed bounded
  health validation;
- `error` with `payload.reason="device_unusable"`: capture is currently
  unavailable. During initial fallback, `payload.error` reports that the
  system-default microphone is being validated. During recovery it reports
  that retry continues in the background. If opening the system-default device
  times out, the circuit breaker instead reports that recovery is paused and
  that the runtime must be restarted; consumers must not assume retry continues
  from the reason alone. This status is orthogonal to the utterance pipeline
  state.

Consumers should ignore unknown states for forward compatibility and inspect
`payload.reason` when handling an `error` status.

Accepted utterances can also be emitted as JSON (`--json` or
`VOICEPI_JSON=1`) and appended to JSONL through `VOICEPI_METRICS_JSONL`. Records
include timing, model/backend, observed acceleration, language confidence,
dictionary changes, injection strategy, and available target metadata.

`VOICEPI_LOG=debug` records lifecycle and backend decisions.
`VOICEPI_LOG=trace` adds detailed hotkey, audio, and session diagnostics. Normal
diagnostics retain output redaction, but Windows trace mode also enables a raw
keyboard hook whose virtual-key and scan codes can reconstruct passwords or
tokens. Treat Windows trace logs as sensitive and share them only privately.

## Platform capability matrix

| Capability | Windows 10/11 | Linux Wayland | Linux X11 |
|---|---|---|---|
| Desktop settings and runtime control | Yes | Yes | Yes |
| Global push-to-talk | Native drivers | evdev with device permission | Native X11 listener |
| Native audio capture | Yes | Yes | Yes |
| Local whisper.cpp | CPU or Vulkan | CPU in published package | CPU in published package |
| Cloud OpenAI-compatible STT | Yes | Yes | Yes |
| Text injection | Native type/paste | Helper chain | Native X11 helpers |
| Restore a previous target | Yes | No portable compositor API | Yes |
| Tray integration | Yes | Desktop control window | Desktop control window |

## Repository ownership

Runtime code lives under `src/rust`, distribution inputs under `packaging`,
automation under `scripts`, and CI/release workflows under `.github/workflows`.
The detailed source boundaries are documented in
[dev/SOURCE_OWNERSHIP.md](dev/SOURCE_OWNERSHIP.md).
