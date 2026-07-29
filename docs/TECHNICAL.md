# whisper-dictate — technical documentation

## Architecture overview

```text
┌────────────────────────────────────────────────────────────────────┐
│                          whisper-dictate                           │
│                                                                    │
│  ┌────────────────┐    ┌─────────────────┐    ┌─────────────────┐  │
│  │  Hotkey        │    │  Audio          │    │  Text           │  │
│  │  detection     │───▶│  capture        │───▶│  injection      │  │
│  └────────────────┘    └─────────────────┘    └─────────────────┘  │
│           │                     │                      │           │
│  evdev (Wayland)        arecord/pipewire       ydotool (Wayland)   │
│  pynput (X11/Win)       sounddevice (X11)      pynput (X11/Win)    │
└────────────────────────────────────────────────────────────────────┘
```

## Source ownership

The current Rust/Python source split and migration guidance are documented in
[SOURCE_OWNERSHIP.md](SOURCE_OWNERSHIP.md).

## End-to-end data flow

```text
User holds hotkey
      │
      ▼
┌─────────────────────────────────────────────────────────────┐
│ HOTKEY DETECTION                                            │
│                                                             │
│  Wayland: evdev reads /dev/input/event* directly            │
│           — global, works in all apps, layout-agnostic      │
│           — requires user in 'input' group                  │
│                                                             │
│  X11/Win: pynput listener via Xorg/Win32 API                │
└───────────────────────────┬─────────────────────────────────┘
                            │ key_down event
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ AUDIO CAPTURE                                               │
│                                                             │
│  Wayland: arecord -D pipewire (S16_LE mono 16 kHz)          │
│           — routes through PipeWire mixer                   │
│           — avoids silence on sof-hda-dsp (Intel laptops)   │
│           — read in ~125 ms chunks via background thread    │
│                                                             │
│  X11/Win: sounddevice (PortAudio) direct ALSA/WASAPI        │
└───────────────────────────┬─────────────────────────────────┘
                            │ key_up event → stop recording
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ PREPROCESSING                                               │
│                                                             │
│  int16 mono 16 kHz frames → float32                         │
│  raw-input gate: minimum dBFS + speech/noise contrast       │
│  accepted input → gain boost toward -20 dBFS                │
│  VAD filter (Silero, threshold 0.3)                         │
│  SNR diagnostics printed per utterance                      │
└───────────────────────────┬─────────────────────────────────┘
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ TRANSCRIPTION — faster-whisper                               │
│                                                             │
│  Backend: VOICEPI_STT_BACKEND=whisper (default)              │
│  Model: large-v3-turbo (default, fastest)                   │
│  Device: NVIDIA GPU (CUDA) if present, else CPU             │
│  beam_size=1, temperature fallback [0.0, 0.2]               │
│  condition_on_previous_text=False  (avoids hallucinations)  │
│  no_speech_threshold=0.45  (lets quiet speech through)      │
│  Cloud: VOICEPI_STT_BACKEND=openai (OpenAI/Groq/custom)      │
│  (Wave 8 of #348 removed the NeMo/Parakeet backend.)         │
└───────────────────────────┬─────────────────────────────────┘
                            │ text string (e.g. "Rødgrød med fløde.")
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ TEXT INJECTION                                              │
│                                                             │
│  Wayland ──────────────────────────────────────────────┐    │
│                                                        │    │
│    For each char: keycode map or ASCII buffer           │    │
│         │                                              │    │
│         ├── ASCII part ──▶ ydotool type -- "..."       │    │
│         │                                              │    │
│         └── DK char ────▶ ydotool key <code>:<press>   │    │
│                            å = 26:1 26:0               │    │
│                            æ = 39:1 39:0               │    │
│                            ø = 40:1 40:0               │    │
│                            Å = 42:1 26:1 26:0 42:0     │    │
│                            Æ = 42:1 39:1 39:0 42:0     │    │
│                            Ø = 42:1 40:1 40:0 42:0     │    │
│                                                        │    │
│  X11/Windows ──────────────────────────────────────────┘    │
│                                                             │
│    auto: paste for fragile Windows terminals, else type     │
│    --paste: pyperclip.copy() + pynput Ctrl+V                │
│    --type:  pynput keyboard.Controller().type()             │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
              text at cursor in focused window
```

After each accepted utterance, the runtime can emit the same structured
event to stdout (`--json` / `VOICEPI_JSON=1`) and/or append it to a JSONL
file (`VOICEPI_METRICS_JSONL=/path/to/file.jsonl`). The event records audio
duration, transcription compute time, real-time factor, model/device,
language confidence, dictionary replacements, injection strategy and target
metadata. This is meant for comparing microphones, models, vocabulary fixes
and injection behaviour without scraping human log lines.

### Engine and backend provenance

`model`, `device`, `compute_type` and `stt_backend` describe the *configured*
stack, and both engines emit them, so on their own they cannot say which code
path served an utterance -- `stt_backend` is `whisper` whether whisper.cpp or
faster-whisper ran, and `device` is usually `auto`. Three additional fields
record what actually happened:

| Field | Values | Resolved from |
|-------|--------|---------------|
| `engine` | `rust-in-process`, `python-worker` | The runtime that produced the record |
| `stt_impl` | `whisper.cpp`, `faster-whisper`, `cloud-openai`, `cloud-groq` | The transcription backend object that ran, not the `stt_backend` setting (which spells both cloud providers `openai`) |
| `stt_accel` | `vulkan`, `cuda`, `cpu`, `unknown` | whisper.cpp's own `whisper_backend_init_gpu:` model-load verdict, or CTranslate2's resolved device. Never the `device` setting |

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
{"event":"status","state":"loading_model","backend":"whisper","model":"large-v3-turbo","device":"cuda","compute_type":"float16"}
{"event":"status","state":"ready","backend":"whisper","model":"large-v3-turbo","device":"cuda","compute_type":"float16","model_load_s":1.234}
{"event":"status","state":"listening"}
```

The Rust supervisor parses only prefixed stderr lines as worker events; all
other stdout/stderr lines remain normal log output.

Runtime configuration can also come from
`%APPDATA%\WhisperDictate\config.json` (or
`${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/config.json`). The Rust UI edits
that file directly and restarts its managed worker when restart-only settings
change. The dictation loop also checks for config changes at recording
boundaries and applies settings that do not require rebuilding the model.

## Rust desktop platform capability matrix

The Rust egui app is the shared desktop control surface for Windows and Linux.
It owns the managed Python runtime process, edits the existing config JSON,
streams logs, and keeps the terminal workflow available through
`whisper-dictate run -- ...`.

| Capability | Windows 10/11 | Linux Wayland | Linux X11 |
|------------|---------------|---------------|-----------|
| Settings UI | Rust UI is the installer Start-menu, desktop, and postinstall launch target | Supported by `scripts/linux/install-rust-ui.sh` and desktop entry | Supported by the same Rust binary and config flow |
| Terminal command | `whisper-dictate.exe run -- ...`; no compatibility script is installed | `whisper-dictate run -- ...` delegates to the installed venv Python when present | Same as Wayland |
| Runtime start/stop/restart | Rust supervisor uses process-tree cleanup through the same worker boundary | Rust supervisor starts/stops/restarts the Python worker and parses worker status events | Same as Wayland |
| Hotkeys | Python `pynput` path remains authoritative | Python `evdev` path remains authoritative because Wayland global hotkeys require input permissions | Python `pynput` path remains authoritative |
| Text injection | Python direct type or clipboard paste remains authoritative, with paste fallback for fragile terminals | Python `ydotool`/`ydotoold` path remains authoritative | Python `pynput`/clipboard path remains authoritative |
| Active-window profiles | Python target detection remains authoritative | Limited by compositor behavior; keep Python fallback and profile metadata when available | Python X11 target detection remains authoritative |
| Tray and autostart | Installer shortcuts launch Rust UI | Desktop entry launches the control window | Desktop entry launches the control window |

Graceful fallback rule: the Rust UI should expose controls for the managed
runtime and config, but platform integrations that are not yet native Rust
stay behind the Python worker until there is tested parity on both Windows and
Linux. This avoids splitting hotkey, injection, and profile behavior between
two implementations during the migration.

Manual smoke procedures before tagging a Rust UI release:

1. `cargo test --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app`
2. `python -m pytest src/python/tests src/tests/python -q`
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

| Character | Keycode | Linux constant   | US key position |
|-----------|---------|------------------|-----------------|
| å / Å     | 26      | KEY_LEFTBRACE    | [               |
| æ / Æ     | 39      | KEY_SEMICOLON    | ;               |
| ø / Ø     | 40      | KEY_APOSTROPHE   | '               |
| shift     | 42      | KEY_LEFTSHIFT    | Left Shift      |

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
check: app version, config validity, the Rust helper, the configured STT
backend and its prerequisites (faster-whisper/CUDA + model cache, or the
cloud API key and reachability), the audio stack, GPU details and free
disk (the probes in `vp_doctor_checks.py`, with heavy deps imported
lazily so `--help` stays instant). On Linux it also covers the Wayland
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
prefers `pipewire`, falling back to `default`, before using
sounddevice as a last resort.

## Hotkey detection — Wayland vs X11

```text
Wayland                          X11 / Windows / macOS
───────────────────────────────  ──────────────────────────────
evdev: open all /dev/input/      pynput: OS keyboard hook
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

| | |
|---|---|
| Primitive | `std::fs::File::try_lock` — `flock(LOCK_EX\|LOCK_NB)` on Linux, `LockFileEx` on Windows |
| Location | `$XDG_RUNTIME_DIR` / `%LOCALAPPDATA%\WhisperDictate` / temp dir, file name tagged with the user |
| Override | `VOICEPI_PTT_LOCK_DIR` (used by tests and by anyone who needs two isolated instances) |
| Released by | handle close: normal exit, panic, `kill -9`, TerminateProcess |
| Holder identity | an unlocked `.owner` sibling file — advisory only, never part of the decision |

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

| Model          | Size   | Speed (CPU) | Accuracy |
|----------------|--------|-------------|----------|
| `large-v3-turbo` | 1.5 GB | fastest     | very good (default) |
| `large-v3`     | 3 GB   | ~3× slower  | marginally better |
| `medium`       | 1.5 GB | faster      | lower, not recommended |

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
