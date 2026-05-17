# whisper-dictate — speak prompts instead of typing them

App-agnostic **push-to-talk dictation**. Hold a key, speak quietly but
clearly, release — the transcribed text is injected into whatever window
has focus: a terminal, an AI chat, an editor, anything. Fully local:
Whisper runs on your own machine, no cloud STT, nothing leaves the box.

This is a **mic → keyboard**, not an AI chat. There is deliberately no
conversation logic — the "AI" (or text field) is whatever app you're
already in. Switching target = just focus a different window.

## Supported platforms

| Platform | Install method | Notes |
|----------|---------------|-------|
| Ubuntu 24.04 / 26.04 — Wayland | Homebrew | Recommended |
| Linux — X11 | Manual | Any distro |
| Windows 10 / 11 | setup.cmd | CPU or NVIDIA GPU |

---

## Ubuntu 24.04 / 26.04 — Wayland

### Install

Requires [Homebrew](https://brew.sh):

```bash
brew tap factusconsulting/tap
brew install whisper-dictate
```

**First run** builds a machine-local venv (`~/.venv-whisper-dictate`) and
downloads the Whisper model (~1.5 GB). Subsequent runs start instantly.

### One-time system setup

Run once after installing — sets up evdev input group, udev rule for
`/dev/uinput`, ydotool, ydotoold daemon, GNOME keyboard layout, and a
login autostart entry:

```bash
bash "$(brew --prefix whisper-dictate)/libexec/ubuntu26.04/setup.sh"
```

Log out and back in after this runs (required for the `input` group to activate).

### Start

```bash
whisper-dictate --key shift_r+ctrl_r --lang da
```

Hold **right Shift + right Ctrl**, speak, release — text appears directly
at the cursor. No clipboard, no paste shortcut.

To start automatically at login, the setup script creates
`~/.config/autostart/whisper-dictate.desktop`. No manual step needed.

---

## Linux — X11

### Install

```bash
git clone https://github.com/FactusConsulting/whisper-dictate.git
cd whisper-dictate
./setup.sh
```

Requires: `python3` ≥ 3.10, `libportaudio2`, `alsa-utils`, `xclip`:

```bash
sudo apt install libportaudio2 alsa-utils xclip
```

### Start

```bash
./setup.sh --key ctrl_r --lang en
```

Or after the venv is built:

```bash
~/.venv-whisper-dictate/bin/python voice_pi.py --key ctrl_r --lang en
```

---

## Windows 10 / 11

### Install

Double-click **`setup.cmd`**, or from PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -File setup.ps1
```

Fetches CPython 3.12 via `winget` if not already installed, builds a
local venv, downloads the Whisper model, and launches. NVIDIA GPU is
used automatically if present.

### Start

Double-click `setup.cmd` again — it launches directly after the first run.

Hold **Right Ctrl**, speak, release — text appears at the cursor.

---

## Use

1. Start whisper-dictate and leave it running.
2. Focus the window where you want text inserted.
3. **Hold the hotkey, speak, release.**
4. ~1–2 s later the text appears at the cursor.
5. **Ctrl+C (or Esc) to quit** — frees GPU VRAM.

## Flags

| Flag | Effect |
|---|---|
| `--key ctrl_r` | hold-to-talk key (`ctrl_r`, `alt_r`, `f9`…) |
| `--key a+b` | chord: hold **both** keys simultaneously, e.g. `shift_r+ctrl_r` |
| `--lang CODE` | spoken-language hint — see [Languages](#languages) |
| `--autodetect` | let Whisper guess the language (less reliable on short speech) |
| `--paste` | inject via clipboard + Ctrl+V on X11/Windows (Wayland always uses direct evdev keycodes) |
| `--no-type` | print transcription only, don't inject (useful for testing) |
| `--model NAME` | Whisper model (default `large-v3-turbo`; env `VOICEPI_MODEL`) |
| `--device D` | `auto`/`cuda`/`cpu` (default `auto`; env `VOICEPI_DEVICE`) |

## Languages

Pass any [ISO 639-1](https://en.wikipedia.org/wiki/List_of_ISO_639-1_codes) code
that Whisper supports to `--lang`. Omit it (or use `--autodetect`) to let
Whisper guess — less reliable on short or soft utterances.

| Code | Language | Code | Language |
|------|----------|------|----------|
| `da` | Danish | `es` | Spanish |
| `en` | English | `pt` | Portuguese |
| `de` | German | `it` | Italian |
| `fr` | French | `ro` | Romanian |
| `sv` | Swedish | `pl` | Polish |
| `nb` | Norwegian Bokmål | `ru` | Russian |
| `nn` | Norwegian Nynorsk | `cs` | Czech |
| `nl` | Dutch | `sk` | Slovak |
| `fi` | Finnish | `hu` | Hungarian |
| `el` | Greek | `uk` | Ukrainian |
| `tr` | Turkish | `ar` | Arabic |
| `zh` | Chinese | `hi` | Hindi |
| `ja` | Japanese | `ko` | Korean |
| `vi` | Vietnamese | `id` | Indonesian |

Whisper large-v3-turbo supports 99 languages in total — the above are the most
commonly used. On Wayland (Ubuntu 26.04), `--lang da` also auto-sets the DK
keyboard layout so that æøå are injected correctly.

## Tuning

| Env var | Default | Effect |
|---|---|---|
| `VOICEPI_TARGET_DBFS` | `-20` | lower (e.g. `-16`) = boost quiet speech harder |
| `VOICEPI_MODEL` | `large-v3-turbo` | `large-v3` = slightly better accuracy, slower |
| `VOICEPI_DEVICE` | `auto` | `cuda`/`cpu` to force; `auto` = NVIDIA if present |
| `VOICEPI_LANG` | _(auto-detect)_ | spoken-language hint (`da`, `en`, `de`, `fr`…) |

The `[cap]` line prints loudness, gain, noise floor and **SNR** per
utterance — `snr` tells you if the mic is the bottleneck: ≳25 dB
excellent, 15–25 dB workable, <15 dB the mic or room is the limit.

## Technical documentation

Architecture, data flow, Wayland injection details, evdev keycode
reference, and audio routing: see [TECHNICAL.md](TECHNICAL.md).

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
