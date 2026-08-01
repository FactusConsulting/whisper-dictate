# whisper-dictate architecture

## Architecture overview

The application is one native Rust product with two entry points:

| Surface | Responsibility |
| --- | --- |
| `whisper-dictate` CLI | Configuration, diagnostics, recording, transcription, and text injection from a terminal. |
| Rust desktop UI | Settings, runtime lifecycle, tray integration, and live diagnostic logs. |

Both surfaces use the same native runtime modules:

1. **Hotkey detection** receives a push-to-talk chord from the platform.
2. **Audio capture** records mono 16 kHz samples through the platform audio stack.
3. **Preprocessing** applies the input gate and gain normalization.
4. **Transcription** runs native whisper.cpp locally or an OpenAI-compatible cloud backend.
5. **Dictionary and post-processing** applies configured terms and replacements.
6. **Text injection** types or pastes the result into the focused application.

## Source ownership

The native Rust ownership model and repository-tooling boundary are documented in
[dev/SOURCE_OWNERSHIP.md](dev/SOURCE_OWNERSHIP.md).

## End-to-end data flow

The runtime flow is:

```text
hotkey down
    -> audio capture
hotkey up
    -> stop the recording boundary
    -> preprocessing
    -> local whisper.cpp or cloud transcription
    -> dictionary/replacements
    -> native text injection
```

Platform-specific implementations are selected inside each module. Wayland
uses evdev and PipeWire/`arecord`; Windows uses native hooks and WASAPI; X11
uses its native listener and audio helpers. The transcription and injection
contracts remain the same on all platforms.

After each accepted utterance, the runtime can emit the same structured
event to stdout (`--json` / `VOICEPI_JSON=1`) and/or append it to a JSONL
file (`VOICEPI_METRICS_JSONL=/path/to/file.jsonl`). The event records audio
duration, transcription compute time, real-time factor, model/device,
language confidence, dictionary replacements, injection strategy and target
metadata. This is meant for comparing microphones, models, vocabulary fixes
and injection behaviour without scraping human log lines.

### Engine and backend provenance

`model`, `device` and `stt_backend` describe the configured stack. Native
whisper.cpp takes numeric precision from the model file. The fields below
record what actually happened:

| Field | Values | Meaning |
| --- | --- | --- |
| `engine` | `rust-in-process` | The runtime that produced the record. |
| `stt_impl` | `whisper.cpp`, `cloud-openai`, `cloud-groq`, `cloud-custom` | The transcription backend object that ran. Cloud providers are distinguished by the base URL host. |
| `stt_accel` | `vulkan`, `cuda`, `cpu`, `unknown` | whisper.cpp's model-load verdict; it is independent of the configured `device` setting. |

`stt_accel` exists to make a silent fallback visible: a Vulkan-linked binary on
a machine with no usable driver loads the model on CPU and says nothing, while
`device` still reads `auto`.

Each engine also names its resolved stack once at startup, at info level:

```text
[runtime] transcribe backend resolved: engine=rust-in-process impl=whisper.cpp accel=vulkan model=large-v3-turbo
```

At startup `accel` is the *plan* (GPU policy plus compiled-in backend), because
whisper.cpp loads its model lazily on the first utterance. The authoritative
per-load verdict is logged as `[whisper] model loaded: ... accel=...` and
stamped on every utterance as `stt_accel`; when the two disagree, the utterance
record is the one telling the truth.

The Rust desktop app/controller also enables a narrower worker event stream
with `VOICEPI_WORKER_EVENTS=1`. These events are compact JSON objects on
stderr prefixed with the `[worker-event]` marker (followed by a space, then the
JSON) so ordinary stdout remains compatible with the terminal workflow. Current
status events use this shape:

```json
{"event":"status","state":"loading_model","backend":"whisper","model":"large-v3-turbo","device":"cuda"}
{"event":"status","state":"ready","backend":"whisper","model":"large-v3-turbo","device":"cuda","model_load_s":1.234}
{"event":"status","state":"listening"}
```

The Rust supervisor parses only prefixed stderr lines as worker events; all
other stdout/stderr lines remain normal log output.

Runtime configuration can also come from
`%APPDATA%\WhisperDictate\config.json` (or
`${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/config.json`). The Rust UI edits
that file directly and restarts its native runtime when restart-only settings
change. The dictation loop also checks for config changes at recording
boundaries and applies settings that do not require rebuilding the model.

## Rust desktop platform capability matrix

The Rust egui app is the shared desktop control surface for Windows and Linux.
It owns the native in-process runtime, edits the existing config JSON, streams
logs, and keeps the terminal workflow available through
`whisper-dictate run -- ...`.

| Area | Windows 10/11 | Linux Wayland | Linux X11 |
| --- | --- | --- | --- |
| Settings UI | Installer Start-menu, desktop, and postinstall launch target. | `scripts/linux/install-rust-ui.sh` and desktop entry. | Same Rust binary and config flow. |
| Terminal command | `whisper-dictate.exe run -- ...`. | `whisper-dictate run -- ...`. | Same as Wayland. |
| Runtime lifecycle | In-process Rust supervisor. | In-process Rust supervisor. | In-process Rust supervisor. |
| Hotkeys | Native Windows drivers. | Native evdev/rdev drivers; Wayland permissions still apply. | Native rdev driver. |
| Text injection | Native direct typing or clipboard paste. | Native `ydotool`, `dotool`, or `wtype` helpers. | Native X11 helpers. |
| Active-window profiles | Native target detection. | Limited by compositor behavior. | Native X11 target detection. |
| Tray and autostart | Installer shortcuts launch the Rust UI. | Desktop entry launches the control window. | Desktop entry launches the control window. |

Reduced builds fail with an actionable missing-feature error. They never
silently select a different runtime.

Manual smoke procedures before tagging a Rust UI release:

1. `cargo test --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app`
2. `cargo test --manifest-path src/rust/Cargo.toml -p whisper-dictate-app`
3. Linux: `scripts/linux/install-rust-ui.sh`, then
   `~/.local/bin/whisper-dictate doctor` and
   `~/.local/bin/whisper-dictate ui`
4. Windows installer changes: build locally with
   `scripts/windows/build-installer.ps1` and report the generated
   `Output\*.exe` and `Output\*.zip`

## Wayland text injection — why evdev keycodes

`ydotool type` on Ubuntu 26.04 (v1.0.4) is not linked against
libxkbcommon and has no XKB layout awareness. Non-ASCII characters
are silently dropped.

`ydotool key` on the same version requires raw Linux input event
codes in `<code>:<pressed>` format. Symbolic names (`KEY_SEMICOLON`,
`ctrl+shift+v`) are accepted with rc=0 but treated as delays — no
key event is sent.

The solution splits text at the DK special characters:

```text
text: "Rødgrød med fløde."

chunk  type      command
──────────────────────────────────────────────────────
"R"    ASCII     ydotool type -- "R"
"ø"    DK char   ydotool key 40:1 40:0
"dgr"  ASCII     ydotool type -- "dgr"
"ø"    DK char   ydotool key 40:1 40:0
"d med fl"  ASCII  ydotool type -- "d med fl"
"ø"    DK char   ydotool key 40:1 40:0
"de."  ASCII     ydotool type -- "de."
```

The GNOME compositor (Mutter) applies the active XKB layout to
ydotoold's uinput virtual keyboard device. With input source set
to `[('xkb', 'dk')]` (done by `packaging/linux/ubuntu26.04/setup.sh`), scancode
40 maps to ø, 39 to æ, 26 to å.

## Evdev keycode reference (DK layout)

| Character | Keycode | Linux constant | US key position |
| --------- | ------- | -------------- | --------------- |
| å / Å   | 26      | KEY_LEFTBRACE  | [               |
| æ / Æ   | 39      | KEY_SEMICOLON  | ;               |
| ø / Ø   | 40      | KEY_APOSTROPHE | '               |
| shift     | 42      | KEY_LEFTSHIFT  | Left Shift      |

Uppercase sequence example for Ø: `42:1 40:1 40:0 42:0`
(shift down → key down → key up → shift up)

## ydotoold daemon

ydotoold is the daemon that owns the `/dev/uinput` virtual keyboard
device. ydotool is the client that sends commands over a Unix socket.

```text
ydotool (client)
    │
    │  Unix socket (~/.ydotool_socket)
    ▼
ydotoold (daemon)
    │
    │  write() to /dev/uinput
    ▼
kernel input subsystem
    │
    ▼
GNOME compositor (Mutter)
    │  applies XKB dk layout
    ▼
focused application
```

The daemon must be started AFTER the GNOME session is running (it
needs the uinput device to be openable by the input group). It is
configured as a systemd user service that starts with the graphical
session.

`whisper-dictate doctor` runs a no-model-load, cross-platform readiness
check: app version, config validity, the configured native STT backend and its
model cache (or cloud API key and reachability), the audio stack, GPU details
and free disk. On Linux it also covers the Wayland
injection path: `evdev`, `ydotool`, `ydotoold`, socket readiness, `input`
group membership, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and readable
`/dev/input/event*` devices.

## Audio — PipeWire routing

```text
Microphone hardware
      │
      ▼
PipeWire (mixer/router)
      │
      ├──▶ arecord -D pipewire  ◀── whisper-dictate uses this
      │         (correct audio)
      │
      └──▶ PortAudio direct ALSA  ◀── bypasses PipeWire → silence
               (sof-hda-dsp devices)
```

whisper-dictate detects available arecord devices at startup and
prefers `pipewire`, falling back to `default`, before using native cpal capture.

## Hotkey detection — Wayland vs X11

```text
Wayland                          X11 / Windows / macOS
───────────────────────────────  ──────────────────────────────
evdev: open all /dev/input/      native OS keyboard hook
event* devices with EV_KEY       (Xorg on Linux, Win32/Quartz)

read raw scan codes               read keysym events
layout-agnostic                   layout-dependent
global (all apps)                 global (all apps)
requires 'input' group            no special permissions

select() loop, 0.5s timeout       background listener thread
chord: track pressed set          chord: track pressed set
```

## Push-to-talk single-owner lock

Only one whisper-dictate process may own the push-to-talk hotkey at a time.
The second one to try is **refused**, with a message naming the PID that
already holds it.

Why it exists: on 2026-07-29 a `whisper-dictate dictate-run` CLI and the
tray GUI both registered F9 — the GUI on the `win_registerhotkey` driver,
the CLI on `rdev`. One key press made both record, both transcribe, and
both inject, and the utterance came out written over itself character by
character in whatever window had focus. Neither process logged anything
wrong, because neither did anything wrong on its own.

Neither driver can detect the other. `RegisterHotKey` only reports a clash
within its own process, and an `rdev` low-level hook is passive and never
conflicts with anything. So the guard sits above both, in
`hotkey::install_hotkey_with_raw_tap` — the single point every backend and
every entry point (`dictate-run`, `hotkey capture`, `self-test hotkey-boot`,
the tray GUI) passes through.

|                 |                                                                                                     |
| --------------- | --------------------------------------------------------------------------------------------------- |
| Primitive       | `std::fs::File::try_lock` — `flock(LOCK_EX\|LOCK_NB)` on Linux, `LockFileEx` on Windows       |
| Location        | `$XDG_RUNTIME_DIR` / `%LOCALAPPDATA%\WhisperDictate` / temp dir, file name tagged with the user |
| Override        | `VOICEPI_PTT_LOCK_DIR` (used by tests and by anyone who needs two isolated instances)             |
| Released by     | handle close: normal exit, panic,`kill -9`, TerminateProcess                                      |
| Holder identity | an unlocked`.owner` sibling file — advisory only, never part of the decision                     |

The lock's lifetime is bound to the open file handle, so it cannot go
stale: the files may survive a crash, but the OS lock cannot outlive the
process that took it. A leftover `.owner` file with no live lock is
ignored.

Ownership tracks the **listening** window, not the process lifetime.
Stopping the runtime in the GUI releases the lock, so a `dictate-run` can
take the chord while the tray app sits idle; starting it again takes
ownership back, and that can now legitimately be refused if something else
claimed the chord in the meantime.

If the lock file cannot be opened at all (read-only runtime directory), the
hotkey installs **anyway** and the diagnostic log records that the guard is
inactive for the session. Refusing dictation to a user with no second
process would be a worse failure than the one being prevented.

## Whisper model selection

| Model              | Size   | Speed (CPU) | Accuracy               |
| ------------------ | ------ | ----------- | ---------------------- |
| `large-v3-turbo` | 1.5 GB | fastest     | very good (default)    |
| `large-v3`       | 3 GB   | ~3× slower | marginally better      |
| `medium`         | 1.5 GB | faster      | lower, not recommended |

`large-v3-turbo` is the right default for CPU dictation: same
encoder quality as `large-v3`, distilled decoder that is 8× faster.

## XKB layout auto-detection priority

When `--lang da` is passed, whisper-dictate sets `XKB_DEFAULT_LAYOUT`
for child processes automatically. The lookup chain:

```text
1. VOICEPI_XKB_LAYOUT env var  (explicit override)
2. XKB_DEFAULT_LAYOUT env var  (already set in environment)
3. /etc/default/keyboard        (system default, skipped if "us")
4. --lang → _LANG_TO_XKB map   (da→dk, de→de, sv→se, fi→fi …)
```
