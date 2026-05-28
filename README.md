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

If Wayland hotkeys or injection fail, run:

```bash
whisper-dictate --doctor
```

It checks `evdev`, `ydotool`, `ydotoold`, the socket, `input` group
membership, session env vars, and readable `/dev/input/event*` devices.

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

---

## Windows 10 / 11

### Install via installer (recommended)

Download the `.exe` installer from
[GitHub Releases](https://github.com/FactusConsulting/whisper-dictate/releases/latest):

- **`whisper-dictate-windows-cpu-setup-<version>.exe`** — works on all machines
- **`whisper-dictate-windows-nvidia-setup-<version>.exe`** — NVIDIA GPU acceleration

Double-click the installer. It installs to `%LOCALAPPDATA%\Programs\WhisperDictate`
(no admin required) and adds the directory to your user PATH.

### Verify the download

Every release publishes **`sha256sums.txt`** (covering all installer
variants) as a release asset. After downloading, confirm the binary is
exactly what CI built:

```powershell
(Get-FileHash .\whisper-dictate-windows-nvidia-setup-<version>.exe -Algorithm SHA256).Hash.ToLower()
```

Each line of `sha256sums.txt` is `filename=<sha256>`. A match means the
binary is bit-for-bit the artifact built by the public
[`windows-installer`](.github/workflows/windows-installer.yml) GitHub Actions
workflow from this source — it has not been tampered with.

> **Antivirus false positives.** The installer is unsigned and bundles a
> push-to-talk dictation tool: a global keyboard hook + microphone capture +
> synthetic keystroke injection. By design that is the same behavioural
> profile as a keylogger, so Microsoft Defender's machine-learning
> heuristics (detection names ending in `!ml`, e.g. `Wacatac`, `Sabsik`) may
> flag it. The installer payload is **only** the text files listed in
> [`installer/whisper-dictate.iss`](installer/whisper-dictate.iss) — the
> `.py`/`.ps1`/`.cmd`/`.md` scripts in this repo, no compiled binary. After
> verifying the SHA256 above you can cross-check on
> [VirusTotal](https://www.virustotal.com/): a handful of heuristic engines
> flag it, the large majority report clean. To avoid the heuristic entirely,
> use the [zip / source install](#install-manually-zip) and run `setup.cmd`
> — identical software, no installer stub.

### Install via winget

Once the [pending PR](https://github.com/microsoft/winget-pkgs/pulls?q=is%3Apr+FactusConsulting.WhisperDictate)
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

On an NVIDIA machine, force CUDA explicitly with:

```powershell
cd C:\source\whisper-dictate
.\setup.cmd --key ctrl_r --lang da --device cuda
```

Hold **Right Ctrl**, speak, release — text appears at the cursor.
NVIDIA GPU is used automatically if present.

---

## Use

1. Start whisper-dictate and leave it running.
2. Focus the window where you want text inserted.
3. **Hold the hotkey, speak, release.**
4. ~1–2 s later the text appears at the cursor.
5. **Ctrl+C (or 3× Esc, configurable via `VOICEPI_QUIT_COUNT`) to quit** — frees GPU VRAM.

## Flags

| Flag | Effect |
|---|---|
| `--key ctrl_r` | hold-to-talk key (`ctrl_r`, `alt_r`, `f9`…; env `VOICEPI_KEY`) |
| `--key a+b` | chord: hold **both** keys simultaneously, e.g. `shift_r+ctrl_r` |
| `--lang CODE` | spoken-language hint — see [Languages](#languages) |
| `--autodetect` | let Whisper guess the language (less reliable on short speech) |
| `--type` | force direct keyboard typing on X11/Windows (env `VOICEPI_INJECT_MODE=type`; Wayland always uses direct evdev keycodes) |
| `--paste` | force clipboard + Ctrl+V on X11/Windows (env `VOICEPI_INJECT_MODE=paste`; Wayland always uses direct evdev keycodes) |
| `--no-type` | print transcription only, don't inject (env `VOICEPI_INJECT_MODE=print`; useful for testing) |
| `--json` | also emit one structured JSON event per utterance (env `VOICEPI_JSON=1`) |
| `--doctor` | run Linux/Wayland health checks and exit |
| `--model NAME` | Whisper model (default `large-v3-turbo`; env `VOICEPI_MODEL`) |
| `--device D` | `auto`/`cuda`/`cpu` (default `auto`; env `VOICEPI_DEVICE`; invalid values are rejected) |

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
| `nb` | Norwegian Bokmål | `uk` | Ukrainian |
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

Every setting — all `VOICEPI_*` env vars **and** CLI flags, their possible
values, defaults, and how to set them per platform (Windows .exe / Homebrew /
Nix / CLI): see **[CONFIGURATION.md](CONFIGURATION.md)**. The most common knobs:

| Env var | Default | Effect |
|---|---|---|
| `VOICEPI_TARGET_DBFS` | `-20` | lower (e.g. `-16`) = boost quiet speech harder |
| `VOICEPI_MIN_INPUT_DBFS` | `-55` | reject raw input quieter than this before Whisper |
| `VOICEPI_MIN_SNR_DB` | `6` | reject raw input without enough speech-vs-noise contrast |
| `VOICEPI_MODEL` | `large-v3-turbo` | `large-v3` = slightly better accuracy, slower |
| `VOICEPI_STT_BACKEND` | `whisper` | `whisper` (default faster-whisper) or `parakeet` (optional NVIDIA NeMo backend) |
| `VOICEPI_DEVICE` | `auto` | `cuda`/`cpu` to force; `auto` = NVIDIA if present |
| `VOICEPI_LANG` | _(auto-detect)_ | spoken-language hint (`da`, `en`, `de`, `fr`…) |
| `VOICEPI_KEY` | `ctrl_r` | hold-to-talk key or chord, e.g. `f9`, `alt_r`, `ctrl_l+space` |
| `VOICEPI_INJECT_MODE` | `auto` | `auto`, `type`, `paste`, or `print`; `auto` types directly except for known fragile Windows terminal targets, where it uses paste |
| `VOICEPI_BEAM_SIZE` | `1` | raise to `5` for better accuracy — 3-4× slower on CPU |
| `VOICEPI_INITIAL_PROMPT` | _(none)_ | context hint for domain-specific terms, e.g. `"Winget, whisper-dictate"` |
| `VOICEPI_DICTIONARY` | user config path | JSON/text dictionary of product names and smart replacements, e.g. `Cloud Code` → `Claude Code` |
| `VOICEPI_COMPUTE_TYPE` | _(default: `int8_float16` on GPU, `int8` on CPU)_ | force precision (`float16`, `bfloat16`, `float32`) — see VRAM table in [CONFIGURATION.md](CONFIGURATION.md) |
| `VOICEPI_DEBUG` | _(unset)_ | `1` → log every effective setting + which env var supplied it at startup (verifies `setx` actually arrived) |
| `VOICEPI_JSON` | _(unset)_ | `1` → print one JSON event per accepted utterance |
| `VOICEPI_METRICS_JSONL` | _(unset)_ | append one JSON metrics event per accepted utterance to this file |
| `VOICEPI_STT_DEBUG` | _(unset)_ | `1` → print Whisper segment metadata for debugging quality |
| `VOICEPI_VAD_THRESHOLD` | `0.3` | Silero VAD speech threshold passed to faster-whisper |
| `VOICEPI_VAD_MIN_SILENCE_MS` | `600` | minimum silence gap used by VAD segmentation |

Dictionary helper commands run before Whisper loads, for example
`setup.ps1 --dictionary-status`, `setup.ps1 --dictionary-open`,
`setup.ps1 --dictionary-add "Claude Code"`, and
`setup.ps1 --dictionary-replace "Cloud Code=Claude Code"`.

Optional Parakeet backend: install `requirements-parakeet.txt`, then set
`VOICEPI_STT_BACKEND=parakeet`. NeMo is imported lazily, so default Whisper
runs and `--doctor` do not need Parakeet dependencies.

The `[gate]` line shows whether the raw input was accepted before gain
boost. The `[cap]` line prints loudness, gain, noise floor and **SNR** per
accepted utterance — `snr` tells you if the mic is the bottleneck: ≳25 dB
excellent, 15–25 dB workable, <15 dB the mic or room is the limit.
The `[stt]` line also includes `rtf` (real-time factor): `0.50` means the
transcription took half as long as the recording, `2.00` means twice as long.

Full reference — every `[cap]`/`[gate]`/`[stt]` field, what good vs bad looks
like, and how to compare two microphones: see
[MICROPHONE.md](MICROPHONE.md).

To test text injection without recording or loading Whisper, focus a target
input field and run `python scripts/inject-smoke.py --mode auto`. Try the same
target with `--mode type` and `--mode paste` when diagnosing app-specific input
behaviour.

## Technical documentation

Architecture, data flow, Wayland injection details, evdev keycode
reference, and audio routing: see [TECHNICAL.md](TECHNICAL.md).

## Tests

Run the fast unit tests with:

```bash
python -m unittest discover -s tests -v
```

## Releasing

Push a version tag:

```bash
git tag v0.2.1 && git push origin v0.2.1
```

This triggers **`release.yml`**: it builds the four zip bundles, publishes
the GitHub Release, and (when the `HOMEBREW_TAP_TOKEN` repo secret is set)
auto-bumps `url`/`sha256` in
[`FactusConsulting/homebrew-tap`](https://github.com/FactusConsulting/homebrew-tap)
`Formula/whisper-dictate.rb`.

The Windows `.exe` installers are built by the release workflow itself.
After the tag is pushed, CI publishes the zip bundles, builds the versioned
installers, uploads them to the Release, and regenerates the local winget
manifests in this repo (used for the `winget install --manifest` install path).

## Wayland keyboard-layout testing status

Wayland text injection uses per-layout evdev keycode maps. Real-hardware
verification is incomplete — help wanted (open an issue/PR with results):

| Layout(s) | Method | Status |
|---|---|---|
| `dk`, `no` | direct keycodes | ✅ verified on real hardware |
| `se`, `de`, `fi` | direct keycodes | ⚠️ implemented, **not** hardware-tested |
| `ua` | direct keycodes (full Cyrillic) | ⚠️ implemented, **not** hardware-tested |
| `es`, `pt`, `br`, `pl` | dead-key / AltGr composition | ⚠️ implemented, **not** hardware-tested; composition is compositor-dependent and the highest-risk path |
| `fr` (AZERTY), `it`, `nl` | — | ❌ **no keycode map** — non-ASCII for these layouts is dropped on Wayland; a `[inject] advarsel:` line lists the affected characters |

Notes:

- `pt` covers **European** Portuguese. Brazilian users get the `br` map only
  when the system keyboard is `br` (auto-detected from `/etc/default/keyboard`)
  or `VOICEPI_XKB_LAYOUT=br` is set — `--lang pt` alone always selects EU `pt`,
  because the spoken-language code is region-less.
- For an unmapped layout, set `VOICEPI_XKB_LAYOUT` to a mapped layout with the
  same physical key positions if one applies, or expect ASCII-only injection.

## License

MIT — see [LICENSE](LICENSE).
