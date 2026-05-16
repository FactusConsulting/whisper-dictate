# whisper-dictate — speak prompts instead of typing them

App-agnostic **push-to-talk dictation**. Hold a key, speak quietly but
clearly, release — the transcribed text is injected into whatever window
has focus: a terminal, an AI chat, an editor, anything. Fully local:
Whisper runs on your own machine, no cloud STT, nothing leaves the box.

This is a **mic → keyboard**, not an AI chat. There is deliberately no
conversation logic — the "AI" (or text field) is whatever app you're
already in. Switching target = just focus a different window.

## Install

### Linux — Homebrew (recommended)

```bash
brew tap factusconsulting/tap
brew install whisper-dictate
```

**First run** builds a machine-local venv (`~/.venv-whisper-dictate`) and
downloads the Whisper model (~1.5 GB). Subsequent runs just launch.

**Wayland (Ubuntu 24.04/26.04) — automated setup:**

```bash
git clone https://github.com/FactusConsulting/whisper-dictate.git
bash whisper-dictate/ubuntu26.04/setup.sh
```

This handles everything: brew install, evdev input group, udev rule for
ydotool, ydotoold daemon, and an autostart entry for GNOME login.

Log out and back in after running, then hold **right Shift + right Ctrl**,
speak, release — text appears in whatever window had focus.

### Linux — manual

```bash
git clone https://github.com/FactusConsulting/whisper-dictate.git
cd whisper-dictate
./setup.sh --paste --key shift_r+ctrl_r   # Wayland
# or
./setup.sh --paste                         # X11
```

Requires: `python3` ≥ 3.10, `libportaudio2`, `alsa-utils`, `wl-clipboard`
(Wayland) or `xclip` (X11):

```bash
sudo apt install libportaudio2 alsa-utils wl-clipboard
```

### Windows — one click

Double-click **`setup.cmd`**, or:

```powershell
powershell -ExecutionPolicy Bypass -File setup.ps1
```

Fetches CPython 3.12 via `winget` if missing, builds venv, downloads model,
launches. Hold **Right Ctrl**, speak, release.

## Use

1. Start whisper-dictate. Leave it running in a terminal.
2. Focus the window where you want text inserted.
3. **Hold the hotkey, speak, release.**
4. ~1–2 s later the text appears at the cursor.
5. **Ctrl+C (or Esc) to quit** — frees GPU VRAM.

## Flags

| Flag | Effect |
|---|---|
| `--key ctrl_r` | hold-to-talk key (`ctrl_r`, `alt_r`, `f9`…) |
| `--key a+b` | chord: hold **both** keys simultaneously, e.g. `shift_r+ctrl_r` |
| `--paste` | inject via clipboard + Ctrl+V — **use this on Wayland** (atomic, no dropped spaces) |
| `--no-type` | just print transcription, don't inject (testing) |
| `--lang da` | spoken-language hint `da`/`en`/`de`/`fr`… (default `da`; env `VOICEPI_LANG`) |
| `--autodetect` | let Whisper guess the language (less reliable on short/soft speech) |
| `--model NAME` | Whisper model (default `large-v3-turbo`; env `VOICEPI_MODEL`) |
| `--device D` | `auto`/`cuda`/`cpu` (default `auto`; env `VOICEPI_DEVICE`) |

## Wayland details (Ubuntu 24.04/26.04)

**Why `--paste` on Wayland:**
Without it, pynput types character-by-character via XWayland, which doesn't
reach Wayland-native windows reliably. `--paste` uses `wl-clipboard` for the
clipboard and injects a single Ctrl+V — atomic and works everywhere.

**Hotkey detection — evdev:**
On Wayland, pynput's Xorg backend only sees keyboard events from XWayland
windows. whisper-dictate detects `WAYLAND_DISPLAY` and switches to reading
`/dev/input/event*` directly via `evdev` — global, layout-agnostic, works
in all apps. Requires the user to be in the `input` group (see Install).

**Audio capture — arecord + PipeWire:**
PortAudio opens the ALSA hardware device directly, bypassing PipeWire's
mixer — the mic reads as silence on `sof-hda-dsp` devices (common on
Intel laptops). whisper-dictate uses `arecord -D pipewire` which routes
through PipeWire correctly. Falls back to direct ALSA on systems without
PipeWire.

**Sample rate:**
Some devices only support 48 kHz. whisper-dictate detects this and
resamples 48 kHz → 16 kHz (Whisper's required rate) using
`scipy.signal.resample_poly`.

## How it works

```
hold hotkey (evdev on Wayland, pynput on X11/Win/Mac)
   │
   ▼ mic open
arecord -D pipewire (Wayland) or sounddevice (X11/Win)
   │
   ▼ release hotkey → stop recording
resample to 16 kHz if needed (scipy)
   │
   ▼
faster-whisper (CPU or NVIDIA GPU)
boost quiet audio → VAD → transcribe
   │
   ▼
wl-clipboard + Ctrl+V (--paste, Wayland)
or pynput type()   (X11/Win)
   │
   ▼
text at cursor in whatever window is focused
```

## Tuning

| Env var | Default | Effect |
|---|---|---|
| `VOICEPI_TARGET_DBFS` | `-20` | lower (e.g. `-16`) = boost quiet speech harder |
| `VOICEPI_MODEL` | `large-v3-turbo` | `large-v3` = slightly better accuracy, slower |
| `VOICEPI_DEVICE` | `auto` | `cuda`/`cpu` to force; `auto` = NVIDIA if present |
| `VOICEPI_LANG` | `da` | spoken-language hint (`en`, `de`, `fr`…) |

The `[cap]` line prints loudness, gain, noise floor and **SNR** per
utterance — `snr` tells you if the mic is the bottleneck: ≳25 dB
excellent, 15–25 dB workable, <15 dB the mic or room is the limit.

## Release variants

`device` auto-detects at runtime: NVIDIA GPU → CUDA, otherwise CPU.

| Release asset | Platform | Engine |
|---|---|---|
| `whisper-dictate-windows-nvidia.zip` | Windows + NVIDIA GPU | CUDA (fast) |
| `whisper-dictate-windows-cpu.zip` | Windows, no NVIDIA | CPU |
| `whisper-dictate-linux-cpu.zip` | Ubuntu 24.04/26.04 | CPU |

## Releasing

Push a version tag — CI builds bundles and publishes the GitHub Release:

```bash
git tag v0.2.1 && git push origin v0.2.1
```

Then bump `url`/`sha256` in
[`FactusConsulting/homebrew-tap`](https://github.com/FactusConsulting/homebrew-tap)
`Formula/whisper-dictate.rb`.

## License

MIT — see [LICENSE](LICENSE).
