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
| NixOS / nix-env | Nix flake | `nix run` or NixOS module |
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

## NixOS / Nix

### Run without installing

```bash
nix run github:FactusConsulting/whisper-dictate -- --key shift_r+ctrl_r --lang da
```

### Install into a profile

```bash
nix profile install github:FactusConsulting/whisper-dictate
whisper-dictate --key shift_r+ctrl_r --lang da
```

### NixOS module (recommended for NixOS users)

Add to your `flake.nix`:

```nix
inputs.whisper-dictate.url = "github:FactusConsulting/whisper-dictate";
```

Then in your NixOS configuration:

```nix
imports = [ inputs.whisper-dictate.nixosModules.default ];

services.whisperDictate = {
  enable = true;
  users  = [ "yourname" ];   # added to the 'input' group
};
```

The module enables `ydotool` (Wayland text injection), adds the udev rule
for `/dev/uinput`, and installs the package system-wide. Log out and back
in after the first activation (required for the `input` group to take effect).

### Official nixpkgs (pending PR)

A PR to [NixOS/nixpkgs](https://github.com/NixOS/nixpkgs) is open. Once
merged, you can install without the flake:

```bash
nix-env -iA nixpkgs.whisper-dictate
# or in configuration.nix:
environment.systemPackages = [ pkgs.whisper-dictate ];
```

---

## Windows 10 / 11

### Install via installer (recommended)

Download the `.exe` installer from
[GitHub Releases](https://github.com/FactusConsulting/whisper-dictate/releases/latest):

- **`whisper-dictate-windows-cpu-setup.exe`** — works on all machines
- **`whisper-dictate-windows-nvidia-setup.exe`** — NVIDIA GPU acceleration

Double-click the installer. It installs to `%LOCALAPPDATA%\Programs\WhisperDictate`
(no admin required) and adds the directory to your user PATH.

### Install via winget

Once the [pending PR](https://github.com/microsoft/winget-pkgs/pull/375681)
to the official winget package index merges:

```powershell
winget install FactusConsulting.WhisperDictate
```

**Until then**, install from this repo's manifests directly:

```powershell
# One-time, in an elevated (admin) PowerShell:
winget settings --enable LocalManifestFiles

# Then (no admin needed):
git clone https://github.com/FactusConsulting/whisper-dictate.git
winget install --manifest .\whisper-dictate\manifests
```

> The installer is not yet code-signed, so Windows SmartScreen warns
> that the publisher is unknown — choose **More info → Run anyway**.
> Do **not** pass `--disable-interactivity`: SmartScreen blocks the
> unsigned installer when it cannot prompt. Passing a raw manifest
> *URL* to `--manifest` does **not** work — winget only accepts a
> local path, not a URL.

### Install manually (zip)

Download the zip from [GitHub Releases](https://github.com/FactusConsulting/whisper-dictate/releases/latest),
unzip anywhere, and double-click **`setup.cmd`**.

First-time setup downloads Python 3.12 via winget (if needed), builds a
local venv, and downloads the Whisper model (~1.5 GB).

### Start

```powershell
setup.cmd --key ctrl_r --lang en
```

Or after first-time setup, launch directly:

```powershell
setup.cmd --key ctrl_r --lang da
```

Hold **Right Ctrl**, speak, release — text appears at the cursor.
NVIDIA GPU is used automatically if present.

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
| `VOICEPI_BEAM_SIZE` | `1` | raise to `5` for better accuracy — 3-4× slower on CPU |
| `VOICEPI_INITIAL_PROMPT` | _(none)_ | context hint for domain-specific terms, e.g. `"Winget, whisper-dictate"` |

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
