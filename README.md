<p align="center">
  <img src="assets/whisper-dictate-logo.svg" width="112" height="112" alt="whisper-dictate logo">
</p>

<h1 align="center">whisper-dictate</h1>

<p align="center"><strong>Speak prompts instead of typing them.</strong></p>

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
| Windows 10 / 11 | Chocolatey or Rust UI installer | CPU or NVIDIA GPU |

Windows quick install with Chocolatey CLI 2.x:

```powershell
choco source add -n=whisper-dictate -s="https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json"
choco install whisper-dictate --source=whisper-dictate -y
```

---

## Ubuntu 24.04 / 26.04 — Wayland

### Install

Requires [Homebrew](https://brew.sh):

```bash
brew tap factusconsulting/tap
brew install whisper-dictate
whisper-dictate setup-ubuntu
```

**First run** builds a machine-local venv (`~/.venv-whisper-dictate`) and
downloads the Whisper model (~1.5 GB). Subsequent runs start instantly.

The setup script installs the Wayland input pieces, creates a GNOME
**Whisper Dictate** launcher, adds login autostart, and opens the Rust UI.
After that, start it from the Ubuntu launcher or run:

```bash
whisper-dictate ui
```

Then press **Start** in the Runtime tab. Hold **right Shift + right Ctrl**,
speak, release.

### One-time system setup details

The setup command above runs once after installing and sets up evdev input group, udev rule for
`/dev/uinput`, ydotool, ydotoold daemon, GNOME keyboard layout, and a
desktop/login autostart entry:

```bash
whisper-dictate setup-ubuntu
```

Log out and back in after this runs (required for the `input` group to activate).

The setup script defaults to Danish (`WD_LANG=da`, XKB layout `dk`). For other
languages pass `WD_LANG` (and optionally `WD_XKB` if the layout code differs from
the language code) before running the setup command — for example
`WD_LANG=de bash packaging/linux/ubuntu26.04/setup.sh`.

### Terminal start

```bash
whisper-dictate run --key shift_r+ctrl_r --lang da
```

Hold **right Shift + right Ctrl**, speak, release — text appears at the
cursor. On Wayland, plain ASCII is typed directly; Unicode text such as
`æøå` is inserted via the clipboard so it works across keyboard layouts.

If Wayland hotkeys or injection fail, run:

```bash
whisper-dictate doctor
```

It checks `evdev`, `ydotool`, `ydotoold`, the socket, `input` group
membership, session env vars, and readable `/dev/input/event*` devices.

To start the UI automatically at login, the setup script creates
`~/.config/autostart/whisper-dictate.desktop`. No manual step needed.
When running headless (`Terminal=false`), enable `VOICEPI_FEEDBACK_SOUNDS` and/or
`VOICEPI_FEEDBACK_NOTIFY` in the Output tab so audio cues and desktop notifications
replace the console diagnostics that would otherwise be swallowed.

---

## Linux — X11

### Install

From a release zip, use the bundled Rust controller:

```bash
unzip whisper-dictate-linux-<version>.zip
cd whisper-dictate
./whisper-dictate install
./whisper-dictate ui
```

From a source checkout:

```bash
git clone https://github.com/FactusConsulting/whisper-dictate.git
cd whisper-dictate
cargo run --manifest-path src/rust/Cargo.toml --target-dir target --release -p whisper-dictate-app -- install
cargo run --manifest-path src/rust/Cargo.toml --target-dir target --release -p whisper-dictate-app -- ui
```

Requires: `python3` ≥ 3.10, `libportaudio2`, `alsa-utils`, `xclip`:

```bash
sudo apt install libportaudio2 alsa-utils xclip
```

### Optional Rust desktop UI

The Rust UI is the cross-platform control surface being introduced for Linux
and Windows. It edits the same config JSON as the Python runtime, starts and
stops dictation directly, streams logs, and restarts its managed runtime when
restart-only settings change.

Install it for your user:

```bash
scripts/linux/install-rust-ui.sh
```

Then run:

```bash
whisper-dictate ui
```

The installer copies the Rust binary to `~/.local/bin/whisper-dictate` and
adds `~/.local/share/applications/whisper-dictate.desktop`.

### Start

```bash
./whisper-dictate run --key ctrl_r --lang en
```

Or from a source checkout after the venv is built:

```bash
cargo run --manifest-path src/rust/Cargo.toml --target-dir target --release -p whisper-dictate-app -- run --key ctrl_r --lang en
```

---

## NixOS / Nix

### Run without installing

```bash
nix run github:FactusConsulting/whisper-dictate -- run --key shift_r+ctrl_r --lang da
```

### Install into a profile

```bash
nix profile install github:FactusConsulting/whisper-dictate
whisper-dictate run --key shift_r+ctrl_r --lang da
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

- **`whisper-dictate-windows-setup-<version>.exe`** — one Windows installer for CPU, NVIDIA GPU, Parakeet and cloud STT

Double-click the installer. It installs to `%LOCALAPPDATA%\Programs\WhisperDictate`
(no admin required), adds the directory to your user PATH, and creates a
Start-menu **whisper-dictate** shortcut. On Windows that shortcut opens the
unified control UI: runtime start/stop/restart, settings, dictionary access and
logs in one window. First-time Install/Repair uses CPU dependencies by default;
when you set Device to `cuda` and run Install/Repair, the same install can add
NVIDIA CUDA dependencies on demand.

### Verify the download

Every release publishes **`sha256sums.txt`** as a release asset. After downloading, confirm the binary is
exactly what CI built:

```powershell
(Get-FileHash .\whisper-dictate-windows-setup-<version>.exe -Algorithm SHA256).Hash.ToLower()
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
> flag it. The installer payload is listed in
> [`packaging/windows/inno/whisper-dictate.iss`](packaging/windows/inno/whisper-dictate.iss): the
> Python runtime files, docs, and the Rust controller
> binary built by CI. After
> verifying the SHA256 above you can cross-check on
> [VirusTotal](https://www.virustotal.com/): a handful of heuristic engines
> flag it, the large majority report clean. To avoid the heuristic entirely,
> build from source and run the Rust controller directly — identical software,
> no installer stub.

### Install via public Chocolatey source

Releases publish a Chocolatey package asset and update a public GitHub Pages
NuGet v3 feed:

- **`whisper-dictate.<version>.nupkg`**

Add the source once, then install or upgrade from any PowerShell terminal:

```powershell
choco source add -n=whisper-dictate -s="https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json"
choco install whisper-dictate --source=whisper-dictate -y
choco upgrade whisper-dictate --source=whisper-dictate -y
```

This source is public and does not require a GitHub account or token. It uses
Chocolatey CLI 2.x NuGet v3 feed support.

The release workflow also publishes the package to GitHub Packages NuGet. That
feed normally requires GitHub credentials:

```powershell
choco source add -n=whisper-dictate-github -s="https://nuget.pkg.github.com/FactusConsulting/index.json" -u="<github-user>" -p="<token>"
choco install whisper-dictate --source=whisper-dictate-github -y
```

The Chocolatey package downloads the matching
`whisper-dictate-windows-setup-<version>.exe` release asset, verifies its
SHA256, and runs the same silent per-user installer as the direct `.exe` path.

Common package commands:

```powershell
# Show configured sources
choco source list

# Install the latest release from our public feed
choco install whisper-dictate --source=whisper-dictate -y

# Upgrade to the latest release from our public feed
choco upgrade whisper-dictate --source=whisper-dictate -y

# Pin the current version if you do not want automatic upgrades
choco pin add -n=whisper-dictate

# Remove the package and run the bundled uninstaller
choco uninstall whisper-dictate -y
```

If Chocolatey reports that it cannot load the service index, verify that you
are on Chocolatey CLI 2.x or newer:

```powershell
choco --version
```

You can also test a downloaded package directly:

```powershell
choco install whisper-dictate --source="C:\path\to\nupkg-folder" -y
```

### Install via local winget manifests

The official `winget-pkgs` path is not currently active; the package was
rejected while the project is still new. Until that can be revisited, each
release ships ready-to-use manifests as the `whisper-dictate-winget-<version>.zip`
asset (already pinned to that release's installer URL and SHA256):

```powershell
# One-time, in an elevated (admin) PowerShell:
winget settings --enable LocalManifestFiles

# Then (no admin needed): download whisper-dictate-winget-<version>.zip from the
# latest release, extract it, and point winget at the extracted folder:
winget install --manifest .\winget
```

Download the asset from
[the latest release](https://github.com/FactusConsulting/whisper-dictate/releases/latest).

> The installer is not yet code-signed, so Windows SmartScreen warns
> that the publisher is unknown — choose **More info → Run anyway**.
> Do **not** pass `--disable-interactivity`: SmartScreen blocks the
> unsigned installer when it cannot prompt. Passing a raw manifest
> *URL* to `--manifest` does **not** work — winget only accepts a
> local path, not a URL.

### Install manually (zip)

Download the zip from [GitHub Releases](https://github.com/FactusConsulting/whisper-dictate/releases/latest),
unzip anywhere, and run **`whisper-dictate.exe install`** once, then
**`whisper-dictate.exe ui`**. The `.exe` installer remains the recommended
Windows path.

First-time setup downloads Python 3.12 via winget (if needed), builds a
local venv, and downloads the Whisper model (~1.5 GB).

### Start

```powershell
whisper-dictate.exe run --key ctrl_r --lang en
```

With the `.exe` installer, use the Start-menu **whisper-dictate** shortcut to
run dictation and settings from one UI. The normal **whisper-dictate** shortcut
runs the Rust UI and starts the Python worker hidden underneath it, with logs
streamed into the Runtime tab.

After first-time setup, launch directly:

```powershell
whisper-dictate run --key ctrl_r --lang da
```

On an NVIDIA machine, force CUDA explicitly with:

```powershell
cd C:\source\whisper-dictate
.\whisper-dictate.exe run --key ctrl_r --lang da --device cuda
```

Hold **Right Ctrl**, speak, release — text appears at the cursor.
NVIDIA GPU is used automatically if present.

---

## Use

1. Start whisper-dictate and leave it running.
2. Focus the window where you want text inserted.
3. **Hold the hotkey, speak, release.**
4. ~1–2 s later the text appears at the cursor.
5. **Ctrl+C (or 3× Esc by default) to quit** — frees GPU VRAM. Configure with `VOICEPI_QUIT_KEY` and `VOICEPI_QUIT_COUNT`.

## Flags

| Flag | Effect |
|---|---|
| `--key ctrl_r` | hold-to-talk key (`ctrl_r`, `alt_r`, `f9`…; env `VOICEPI_KEY`) |
| `--key a+b` | chord: hold **both** keys simultaneously, e.g. `shift_r+ctrl_r` |
| `VOICEPI_TOGGLE=1` | toggle mode (no CLI flag — set the env var or the **Toggle mode** checkbox in the Speech tab, persisted as `toggle_mode` in config.json): **press** the hotkey to start recording, **press again** to stop and transcribe — instead of holding it. Key autorepeat is ignored (rising-edge only); restart-only. |
| `--lang CODE` | spoken-language hint — see [Languages](#languages) |
| `--autodetect` | let Whisper guess the language (less reliable on short speech) |
| `--type` | force direct keyboard typing (env `VOICEPI_INJECT_MODE=type`; on Wayland this uses evdev keycodes and depends on the configured XKB layout for non-ASCII) |
| `--paste` | force clipboard paste (env `VOICEPI_INJECT_MODE=paste`; on Wayland terminal/unknown targets use Ctrl+Shift+V, other known targets use Ctrl+V) |
| `--no-type` | print transcription only, don't inject (env `VOICEPI_INJECT_MODE=print`; useful for testing) |
| `--json` | also emit one structured JSON event per utterance (env `VOICEPI_JSON=1`) |
| `--doctor` | run a cross-platform readiness check (version, config, STT backend + deps, audio, GPU, cloud reachability, disk; plus Linux/Wayland injection checks) and exit |
| `model-capacity` | show local GPU VRAM and which local models can fit |
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
Nix / CLI): see **[docs/CONFIGURATION.md](docs/CONFIGURATION.md)**. The most common knobs:

| Env var | Default | Effect |
|---|---|---|
| `VOICEPI_TARGET_DBFS` | `-20` | lower (e.g. `-16`) = boost quiet speech harder |
| `VOICEPI_MIN_INPUT_DBFS` | `-55` | reject raw input quieter than this before Whisper |
| `VOICEPI_MIN_SNR_DB` | `6` | reject raw input without enough speech-vs-noise contrast |
| `VOICEPI_MODEL` | `large-v3-turbo` | `large-v3` = slightly better accuracy, slower |
| `VOICEPI_STT_BACKEND` | `whisper` | `whisper`, `parakeet`, or explicit opt-in `openai` external transcription |
| `VOICEPI_STT_MODEL` | _(unset)_ | external transcription model, for example `gpt-4o-mini-transcribe` |
| `VOICEPI_STT_BASE_URL` | `https://api.openai.com/v1` | OpenAI-compatible transcription API base URL |
| `VOICEPI_STT_API_KEY` | _(unset)_ | optional external transcription key; `GROQ_API_KEY` works for Groq URLs and `OPENAI_API_KEY` also works |
| `VOICEPI_PARAKEET_MODEL` | `nvidia/parakeet-tdt-0.6b-v3` | Parakeet model; v3 is best default for Danish/mixed Danish-English, TDT 1.1B is for pure English quality tests |
| `VOICEPI_DEVICE` | `auto` | `cuda`/`cpu` to force; `auto` = NVIDIA if present |
| `VOICEPI_LANG` | _(auto-detect)_ | spoken-language hint (`da`, `en`, `de`, `fr`…) |
| `VOICEPI_KEY` | `ctrl_r` | hold-to-talk key or chord, e.g. `f9`, `alt_r`, `ctrl_l+space` |
| `VOICEPI_INJECT_MODE` | `auto` | `auto`, `type`, `paste`, or `print`; `auto` types directly where safe and uses paste for layout-sensitive text, including Unicode on Wayland |
| `VOICEPI_FORMAT_COMMANDS` | `off` | optional spoken formatting commands: `off`, `en`, `da`, or `both` |
| `VOICEPI_BEAM_SIZE` | `1` | raise to `5` for better accuracy — 3-4× slower on CPU |
| `VOICEPI_INITIAL_PROMPT` | _(none)_ | context hint for domain-specific terms, e.g. `"Winget, whisper-dictate"` |
| `VOICEPI_DICTIONARY` | user config path | JSON/text dictionary of product names and smart replacements, e.g. `Cloud Code` → `Claude Code` |
| `VOICEPI_COMPUTE_TYPE` | _(default: `int8_float16` on GPU, `int8` on CPU)_ | force precision (`float16`, `bfloat16`, `float32`) — see VRAM table in [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |
| `VOICEPI_DEBUG` | _(unset)_ | `1` → log every effective setting + which env var supplied it at startup (verifies `setx` actually arrived) |
| `VOICEPI_JSON` | _(unset)_ | `1` → print one JSON event per accepted utterance |
| `VOICEPI_METRICS_JSONL` | _(unset)_ | append one JSON metrics event per accepted utterance to this file |
| `VOICEPI_COMMAND_HOOK` | _(unset)_ | advanced opt-in command hook; receives one utterance JSON event on stdin with no shell interpolation |
| `VOICEPI_COMMAND_HOOK_TIMEOUT_MS` | `2000` | maximum time to wait for the command hook before logging a non-fatal timeout |
| `VOICEPI_HISTORY_ENABLED` | `1` | store accepted live dictations in local history |
| `VOICEPI_HISTORY_JSONL` | user state path | override the local history JSONL path |
| `VOICEPI_LOCAL_ONLY` | _(unset)_ | `1` → block cloud/BYOK backends and force model libraries offline; local models must already be downloaded |
| `VOICEPI_POST_PROCESSOR` | `none` | `ollama`, `openai`, or `groq` → run optional second text pass after STT/dictionary |
| `VOICEPI_POST_MODE` | `raw` | `clean`, `prompt`, `terminal`, `slack`, `email`, `bullets`; `bullet-list` is accepted as an alias; `raw` keeps current behavior |
| `VOICEPI_POST_MODEL` | `qwen2.5:3b` | local Ollama model or OpenAI-compatible chat model |
| `VOICEPI_POST_BASE_URL` | `http://localhost:11434` / `https://api.openai.com/v1` | Ollama or OpenAI-compatible chat endpoint |
| `VOICEPI_POST_API_KEY` | _(unset)_ | optional external post-processing key; the Rust UI can store it in the OS credential store |
| `VOICEPI_POST_TIMEOUT_MS` | `2000` | fallback to dictionary-final text if local rewrite is too slow |
| `VOICEPI_POST_REDACT` | _(unset)_ | opt-in local redaction before cloud post-processing |
| `VOICEPI_POST_REDACT_TERMS` | _(unset)_ | comma-separated names/terms to redact before cloud post-processing |
| `VOICEPI_AUDIO_DUCKING` | _(unset)_ | Windows-only: temporarily lower other app audio while recording |
| `VOICEPI_AUDIO_DUCKING_LEVEL` | `0.25` | target volume for other apps during audio ducking |
| `VOICEPI_STT_DEBUG` | _(unset)_ | `1` → print Whisper segment metadata for debugging quality |
| `VOICEPI_NO_COLOR` / `NO_COLOR` | _(unset)_ | any non-empty value → keep interactive terminal status lines plain |
| `VOICEPI_VAD_THRESHOLD` | `0.3` | Silero VAD speech threshold passed to faster-whisper |
| `VOICEPI_VAD_MIN_SILENCE_MS` | `600` | minimum silence gap used by VAD segmentation |
| `VOICEPI_PARAKEET_MIN_SECONDS` | `1.5` | ignore very short Parakeet captures where language detection is weak |
| `VOICEPI_RELEASE_TAIL_MS` | `200` | keep capturing briefly after hotkey release to avoid clipping final words |
| `VOICEPI_PREVIEW_SECONDS` | `3` | local Whisper only: while recording, show a live partial transcription in the Runtime card every N seconds (`0` disables); display-only, the final text is unchanged |

Dictionary helper commands are handled by the Rust controller before Python or
Whisper loads, for example `whisper-dictate dictionary status`,
`whisper-dictate dictionary open`,
`whisper-dictate dictionary add "Claude Code"`, and
`whisper-dictate dictionary replace "Cloud Code=Claude Code"`. Use
`whisper-dictate run --dictionary-suggest benchmark\results.jsonl` to inspect benchmark
or history JSONL and print suggested smart replacements without mutating the
dictionary. In the Windows Settings UI, open the Dictionary tab and use
Benchmark suggestions to preview the same suggestions and apply the shown
replacement candidates to the configured dictionary.

Optional Parakeet backend: install `requirements/parakeet.txt`, then set
`VOICEPI_STT_BACKEND=parakeet`. NeMo is imported lazily, so default Whisper
runs and `--doctor` do not need Parakeet dependencies. The UI lists only the
practical Parakeet models: 0.6B v3 for Danish/mixed Danish-English, TDT 1.1B
for pure English quality experiments, and 0.6B v2 as a fast English-only
baseline.

Optional external API backends are explicit opt-in. In the Rust UI, choose
`STT backend = openai`, then choose `Cloud STT provider = Groq` or `OpenAI`.
The UI can save the selected provider in `config.json` and save the STT API key
in the OS credential store. From a terminal, set `VOICEPI_STT_BACKEND=openai`
for OpenAI-compatible audio transcription, and set `OPENAI_API_KEY`,
`GROQ_API_KEY`, or `VOICEPI_STT_API_KEY`. Use
`VOICEPI_STT_MODEL=gpt-4o-mini-transcribe`/`gpt-4o-transcribe` for OpenAI, or
`whisper-large-v3-turbo`/`whisper-large-v3` for Groq. For external text cleanup, choose
`Post processor = Groq` or `OpenAI` in the Rust UI Output tab, choose a chat
model, and save a separate Post API key if needed. Terminal runs can set
`VOICEPI_POST_PROCESSOR=groq` or `openai`, `VOICEPI_POST_MODEL=<chat-model>` and
`VOICEPI_POST_API_KEY`, `GROQ_API_KEY`, or `OPENAI_API_KEY`. `VOICEPI_LOCAL_ONLY=1` blocks these
external providers before requests are made — except a self-hosted STT endpoint
on a loopback URL (`localhost`/`127.0.0.1`/`::1`), which stays on the machine and
is therefore allowed. See "Self-host the STT model in a container" below.

For cloud text cleanup, `VOICEPI_POST_REDACT=1` can redact emails, phone
numbers, common API tokens and comma-separated `VOICEPI_POST_REDACT_TERMS`
locally before the OpenAI-compatible post-processing request. Placeholders are
restored in the returned text when possible, and metrics record only placeholder
metadata, not the original sensitive values.

On Windows, `VOICEPI_AUDIO_DUCKING=1` lowers other app audio while the
push-to-talk key is held, then restores volumes before transcription continues.
It is disabled by default and uses `VOICEPI_AUDIO_DUCKING_LEVEL=0.25` unless
configured otherwise.

Groq is available as an opt-in cloud STT provider in the Rust UI. It uses
Groq's OpenAI-compatible transcription endpoint and `whisper-large-v3-turbo`:

```powershell
setx GROQ_API_KEY "gsk_..."
setx VOICEPI_STT_BACKEND openai
setx VOICEPI_STT_BASE_URL https://api.groq.com/openai/v1
setx VOICEPI_STT_MODEL whisper-large-v3-turbo
```

### Self-host the STT model in a container

Only the heavy Whisper model can be containerized — the desktop app
(microphone capture, the global push-to-talk hotkey and injecting text into the
focused window) is host integration and stays on your machine. Run any
OpenAI-compatible Whisper server (e.g. faster-whisper-server or speaches) in a
container and point whisper-dictate at it:

```powershell
docker compose -f packaging/docker/docker-compose.yml up -d
```

Then in the Rust UI Speech tab choose `Speech engine = Cloud STT`,
`Cloud STT provider = Custom (OpenAI-compatible)`, set the API URL to
`http://localhost:8000/v1` and the model to whatever the server preloads (e.g.
`Systran/faster-whisper-large-v3`). No API key is required for a local server.
The **Custom** provider's base URL and model are never normalized away, unlike
the Groq/OpenAI presets. Because the endpoint is loopback, `VOICEPI_LOCAL_ONLY=1`
still permits it (nothing leaves the machine); a non-loopback URL stays blocked.
Terminal runs can set this without the UI:

```powershell
setx VOICEPI_STT_BACKEND openai
setx VOICEPI_STT_BASE_URL http://localhost:8000/v1
setx VOICEPI_STT_MODEL Systran/faster-whisper-large-v3
```

STT API keys saved from the Rust UI are stored in the OS credential store and
passed only to the managed Python worker process as `VOICEPI_STT_API_KEY`.
They are not written to `config.json` or shown in runtime command logs.
Terminal runs can still read keys from the process/user environment
(`OPENAI_API_KEY`, `GROQ_API_KEY`, `VOICEPI_STT_API_KEY`). Post-processing
cloud keys can also be saved from the Rust UI Output tab and are passed to the
worker as `VOICEPI_POST_API_KEY`; if no post key is saved, the worker can fall
back to the loaded STT key.

File transcription for benchmarks/debugging uses the same backend, dictionary
and replacement pipeline as live dictation:

```powershell
whisper-dictate run --transcribe-file sample.wav
whisper-dictate run --transcribe-file sample.wav --json
whisper-dictate run --benchmark-files sample.wav `
  --benchmark-backends "whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3" `
  --benchmark-jsonl benchmark.jsonl
whisper-dictate run --benchmark-corpus benchmark\corpus.json `
  --benchmark-backends "whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3" `
  --benchmark-jsonl benchmark\results.jsonl
whisper-dictate run --calibrate-mic 5
whisper-dictate run --calibrate-file sample.wav --json
whisper-dictate history last
whisper-dictate run --history-copy-last
whisper-dictate run --dictionary-suggest benchmark\results.jsonl --json
```

16-bit WAV works without extra tools. Other formats such as mp3/m4a require
`ffmpeg` on `PATH`. Benchmark mode runs each backend/model spec in an isolated
child process and writes one JSONL result per file/backend, including failures.
The corpus manifest in `benchmark/corpus.json` adds reference text, WER/CER and
expected technical-term hits/misses. Record missing local corpus audio with
`py -3.12 scripts\benchmark\record-corpus.py --manifest benchmark\corpus.json --seconds 7`.
Calibration prints raw dBFS, noise floor, SNR, peak and recommended audio
threshold settings without loading an STT model.
`whisper-dictate model-capacity` prints NVIDIA GPU free/total VRAM and
which Whisper, Parakeet and local Ollama post-processing models can fit now or
after freeing VRAM. The Windows Settings UI exposes the same check on the Core
tab as **Model fit**.

Local history stores accepted live dictations as JSONL. It is local-only,
disabled with `VOICEPI_HISTORY_ENABLED=0`, and can be used to recover the last
transcript if injection/focus failed.

Advanced automation can run a command after each accepted live utterance:

```powershell
setx VOICEPI_COMMAND_HOOK '["python","D:\scripts\handle-dictation.py"]'
setx VOICEPI_COMMAND_HOOK_TIMEOUT_MS 2000
```

The hook receives the same structured utterance event as JSON on stdin. It is
started directly with `shell=False`; transcript text is never interpolated into
a shell command. Prefer the JSON-array form above for exact arguments. Hook
errors/timeouts are logged and recorded in metrics/history, but dictation text
is still injected normally.

Target profiles can be added to `%APPDATA%\WhisperDictate\config.json` under a
`profiles` array. Profiles match active window title/process substrings and
override settings for that utterance, for example using paste mode and an
English technical prompt in Claude Code/Codex terminals while keeping defaults
elsewhere. On Windows, the Profiles tab's **List open windows** button fetches
the live list of visible windows so you can pick one and insert a matching
profile without hand-typing the match values. Window enumeration is Windows
only — Wayland cannot enumerate windows.

Spoken formatting commands are deterministic and opt-in. Set
`VOICEPI_FORMAT_COMMANDS=en`, `da`, or `both` globally or in a target profile.
Examples such as `first item comma new line second item period` or `første
punkt komma ny linje andet punkt punktum` become formatted punctuation and
line breaks. The feature is off by default so literal dictation of words like
`comma` or `punktum` is not changed unless you explicitly enable it.

Desktop UI: on Windows, use the Start-menu **whisper-dictate** shortcut. On
Linux, install the Rust UI with `scripts/linux/install-rust-ui.sh` and run
`whisper-dictate ui`. The UI owns the dictation process, shows the runtime log,
saves settings and restarts dictation when backend/model/device/hotkey settings
change. It writes `%APPDATA%\WhisperDictate\config.json` on Windows and
`${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/config.json` on Linux.

The `[gate]` line shows whether the raw input was accepted before gain
boost. The `[cap]` line prints loudness, gain, noise floor and **SNR** per
accepted utterance — `snr` tells you if the mic is the bottleneck: ≳25 dB
excellent, 15–25 dB workable, <15 dB the mic or room is the limit.
The `[stt]` line also includes `rtf` (real-time factor): `0.50` means the
transcription took half as long as the recording, `2.00` means twice as long.

Full reference — every `[cap]`/`[gate]`/`[stt]` field, what good vs bad looks
like, and how to compare two microphones: see
[docs/MICROPHONE.md](docs/MICROPHONE.md).

To test text injection without recording or loading Whisper, focus a target
input field and run `python scripts/dev/inject-smoke.py --mode auto`. Try the same
target with `--mode type` and `--mode paste`; on Wayland, test both a normal
text editor and a terminal because terminals use a different paste shortcut.

## Technical documentation

Architecture, data flow, Wayland injection details, evdev keycode
reference, and audio routing: see [docs/TECHNICAL.md](docs/TECHNICAL.md).
For the current Rust/Python source split and migration guidance, see
[docs/SOURCE_OWNERSHIP.md](docs/SOURCE_OWNERSHIP.md).

## Tests

Run the fast Python unit tests with:

```bash
python -m pip install pytest numpy
python -m pytest src/python/tests src/tests/python -q
```

For the Rust suite, clippy/fmt, or a CI-matched environment on any OS, use the
dev container — see [CONTRIBUTING.md](CONTRIBUTING.md).

## Releasing

Bump the four version files together (VERSION, `src/rust/Cargo.toml`,
`src/rust/Cargo.lock`, `nix/package.nix` — they must always agree; the script
validates every file before writing anything and re-verifies after), merge
that as a PR, then push a version tag:

```bash
python scripts/dev/bump_version.py X.Y.Z   # or --check to verify consistency
git tag vX.Y.Z && git push origin vX.Y.Z
```

This triggers **`release.yml`**: it publishes the Linux bundle and Rust UI
binary, then builds the unified Windows installer, portable Windows ZIP bundle
and Chocolatey `.nupkg` on a Windows runner. It also publishes the GitHub Release and (when the
`HOMEBREW_TAP_TOKEN` repo secret is set)
auto-bumps `url`/`sha256` in
[`FactusConsulting/homebrew-tap`](https://github.com/FactusConsulting/homebrew-tap)
`Formula/whisper-dictate.rb`.

The Windows `.exe` installer, Windows ZIP bundle and Chocolatey package are
built by the release workflow itself. After the tag is pushed, CI uploads them
to the Release and regenerates the local winget manifests in this repo (used
for the `winget install --manifest` install path). CI publishes the `.nupkg` to
the public GitHub Pages Chocolatey feed at
`https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json` and
to GitHub Packages NuGet. If the `CHOCOLATEY_NUGET_SOURCE` and
`CHOCOLATEY_NUGET_API_KEY` repo secrets are also set, CI pushes the same package
to that additional private Chocolatey/NuGet feed.

For a faster local Windows test loop without creating a release:

```powershell
.\scripts\windows\build-installer.ps1
```

The local installer and portable ZIP are written to `Output\`. The script uses
Inno Setup 6 and installs it via winget, or Chocolatey as a fallback, when it is
missing. Local builds default to the latest release version plus unique semver
build metadata, for example `<version>+local.<timestamp>.g<sha>.dirty`, so
they are easy to distinguish from online releases and from each other. The
installer keeps a numeric Windows file version internally, for example
`<major>.<minor>.<patch>.1`.

## Wayland keyboard-layout testing status

Wayland text injection uses per-layout evdev keycode maps. Real-hardware
verification is incomplete — help wanted (open an issue/PR with results):

| Layout(s) | Method | Status |
|---|---|---|
| `dk`, `no` | direct keycodes | ✅ verified on real hardware |
| `se`, `de`, `fi` | direct keycodes | ⚠️ implemented, **not** hardware-tested |
| `ua` | direct keycodes (full Cyrillic) | ⚠️ implemented, **not** hardware-tested |
| `es`, `pt`, `br`, `pl` | dead-key / AltGr composition | ⚠️ implemented, **not** hardware-tested; composition is compositor-dependent and the highest-risk path |
| `fr` (AZERTY), `it` | dedicated-key + dead-key composition | ⚠️ implemented, **not** hardware-tested; lowercase accents (plus circumflex/diaeresis on `fr`) are mapped. A few uppercase accents that AZERTY/Italian have no simple key for fall back to Unicode `type` |
| `nl` | — | ❌ **no keycode map** — Dutch typically uses US-International; set `VOICEPI_XKB_LAYOUT` to a mapped layout if one matches your hardware, otherwise non-ASCII falls back to Unicode `type` |

Notes:

- `pt` covers **European** Portuguese. Brazilian users get the `br` map only
  when the system keyboard is `br` (auto-detected from `/etc/default/keyboard`)
  or `VOICEPI_XKB_LAYOUT=br` is set — `--lang pt` alone always selects EU `pt`,
  because the spoken-language code is region-less.
- For an unmapped layout, set `VOICEPI_XKB_LAYOUT` to a mapped layout with the
  same physical key positions if one applies, or expect ASCII-only injection.

## License

MIT — see [LICENSE](LICENSE).
