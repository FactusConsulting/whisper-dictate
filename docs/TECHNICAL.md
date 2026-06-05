# whisper-dictate — technical documentation

## Architecture overview

```
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

## End-to-end data flow

```
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
│ TRANSCRIPTION — faster-whisper or optional Parakeet          │
│                                                             │
│  Backend: VOICEPI_STT_BACKEND=whisper (default)              │
│  Model: large-v3-turbo (default, fastest)                   │
│  Device: NVIDIA GPU (CUDA) if present, else CPU             │
│  beam_size=1, temperature fallback [0.0, 0.2]               │
│  condition_on_previous_text=False  (avoids hallucinations)  │
│  no_speech_threshold=0.45  (lets quiet speech through)      │
│  Optional: VOICEPI_STT_BACKEND=parakeet loads NeMo lazily    │
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

The Rust desktop app/controller also enables a narrower worker event stream
with `VOICEPI_WORKER_EVENTS=1`. These events are compact JSON objects on
stderr prefixed with `[worker-event] ` so ordinary stdout remains compatible
with the terminal workflow. Current status events use this shape:

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
| Tray and autostart | Installer shortcuts launch Rust UI | No Rust tray requirement for first Linux release; desktop entry launches the control window | No Rust tray requirement for first Linux release |

Graceful fallback rule: the Rust UI should expose controls for the managed
runtime and config, but platform integrations that are not yet native Rust
stay behind the Python worker until there is tested parity on both Windows and
Linux. This avoids splitting hotkey, injection, and profile behavior between
two implementations during the migration.

Manual smoke procedures before tagging a Rust UI release:

1. `cargo test -p whisper-dictate-app`
2. `python -m pytest tests -q`
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

```
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
to `[('xkb', 'dk')]` (done by `ubuntu26.04/setup.sh`), scancode
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

```
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

`whisper-dictate doctor` runs a no-model-load health check for the
Wayland path: `evdev`, `ydotool`, `ydotoold`, socket readiness, `input`
group membership, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and readable
`/dev/input/event*` devices.

## Audio — PipeWire routing

```
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

```
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

```
1. VOICEPI_XKB_LAYOUT env var  (explicit override)
2. XKB_DEFAULT_LAYOUT env var  (already set in environment)
3. /etc/default/keyboard        (system default, skipped if "us")
4. --lang → _LANG_TO_XKB map   (da→dk, de→de, sv→se, fi→fi …)
```
