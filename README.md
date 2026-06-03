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
| Windows 10 / 11 | Rust UI installer | CPU or NVIDIA GPU |

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

### Terminal start

```bash
whisper-dictate run -- --key shift_r+ctrl_r --lang da
```

Hold **right Shift + right Ctrl**, speak, release — text appears directly
at the cursor. No clipboard, no paste shortcut.

If Wayland hotkeys or injection fail, run:

```bash
whisper-dictate --doctor
```

It checks `evdev`, `ydotool`, `ydotoold`, the socket, `input` group
membership, session env vars, and readable `/dev/input/event*` devices.

To start the UI automatically at login, the setup script creates
`~/.config/autostart/whisper-dictate.desktop`. No manual step needed.

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
cargo run --release -p whisper-dictate-app -- install
cargo run --release -p whisper-dictate-app -- ui
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
scripts/install-linux-rust-ui.sh
```

Then run:

```bash
whisper-dictate ui
```

The installer copies the Rust binary to `~/.local/bin/whisper-dictate` and
adds `~/.local/share/applications/whisper-dictate.desktop`.

### Start

```bash
./whisper-dictate run -- --key ctrl_r --lang en
```

Or from a source checkout after the venv is built:

```bash
cargo run --release -p whisper-dictate-app -- run -- --key ctrl_r --lang en
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
> [`installer/whisper-dictate.iss`](installer/whisper-dictate.iss): the
> Python runtime files, docs, and the Rust controller
> binary built by CI. After
> verifying the SHA256 above you can cross-check on
> [VirusTotal](https://www.virustotal.com/): a handful of heuristic engines
> flag it, the large majority report clean. To avoid the heuristic entirely,
> build from source and run the Rust controller directly — identical software,
> no installer stub.

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
| `--lang CODE` | spoken-language hint — see [Languages](#languages) |
| `--autodetect` | let Whisper guess the language (less reliable on short speech) |
| `--type` | force direct keyboard typing on X11/Windows (env `VOICEPI_INJECT_MODE=type`; Wayland always uses direct evdev keycodes) |
| `--paste` | force clipboard + Ctrl+V on X11/Windows (env `VOICEPI_INJECT_MODE=paste`; Wayland always uses direct evdev keycodes) |
| `--no-type` | print transcription only, don't inject (env `VOICEPI_INJECT_MODE=print`; useful for testing) |
| `--json` | also emit one structured JSON event per utterance (env `VOICEPI_JSON=1`) |
| `--doctor` | run Linux/Wayland health checks and exit |
| `--model-capacity` | show local GPU VRAM and which local models can fit |
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
| `VOICEPI_STT_BACKEND` | `whisper` | `whisper`, `parakeet`, or explicit opt-in `openai` external transcription |
| `VOICEPI_STT_MODEL` | _(unset)_ | external transcription model, for example `gpt-4o-mini-transcribe` |
| `VOICEPI_STT_BASE_URL` | `https://api.openai.com/v1` | OpenAI-compatible transcription API base URL |
| `VOICEPI_STT_API_KEY` | _(unset)_ | optional external transcription key; `GROQ_API_KEY` works for Groq URLs and `OPENAI_API_KEY` also works |
| `VOICEPI_PARAKEET_MODEL` | `nvidia/parakeet-tdt-0.6b-v3` | Parakeet model; v3 is best default for Danish/mixed Danish-English, TDT 1.1B is for pure English quality tests |
| `VOICEPI_DEVICE` | `auto` | `cuda`/`cpu` to force; `auto` = NVIDIA if present |
| `VOICEPI_LANG` | _(auto-detect)_ | spoken-language hint (`da`, `en`, `de`, `fr`…) |
| `VOICEPI_KEY` | `ctrl_r` | hold-to-talk key or chord, e.g. `f9`, `alt_r`, `ctrl_l+space` |
| `VOICEPI_INJECT_MODE` | `auto` | `auto`, `type`, `paste`, or `print`; `auto` types directly except for known fragile Windows terminal targets, where it uses paste |
| `VOICEPI_FORMAT_COMMANDS` | `off` | optional spoken formatting commands: `off`, `en`, `da`, or `both` |
| `VOICEPI_BEAM_SIZE` | `1` | raise to `5` for better accuracy — 3-4× slower on CPU |
| `VOICEPI_INITIAL_PROMPT` | _(none)_ | context hint for domain-specific terms, e.g. `"Winget, whisper-dictate"` |
| `VOICEPI_DICTIONARY` | user config path | JSON/text dictionary of product names and smart replacements, e.g. `Cloud Code` → `Claude Code` |
| `VOICEPI_COMPUTE_TYPE` | _(default: `int8_float16` on GPU, `int8` on CPU)_ | force precision (`float16`, `bfloat16`, `float32`) — see VRAM table in [CONFIGURATION.md](CONFIGURATION.md) |
| `VOICEPI_DEBUG` | _(unset)_ | `1` → log every effective setting + which env var supplied it at startup (verifies `setx` actually arrived) |
| `VOICEPI_JSON` | _(unset)_ | `1` → print one JSON event per accepted utterance |
| `VOICEPI_METRICS_JSONL` | _(unset)_ | append one JSON metrics event per accepted utterance to this file |
| `VOICEPI_COMMAND_HOOK` | _(unset)_ | advanced opt-in command hook; receives one utterance JSON event on stdin with no shell interpolation |
| `VOICEPI_COMMAND_HOOK_TIMEOUT_MS` | `2000` | maximum time to wait for the command hook before logging a non-fatal timeout |
| `VOICEPI_HISTORY_ENABLED` | `1` | store accepted live dictations in local history |
| `VOICEPI_HISTORY_JSONL` | user state path | override the local history JSONL path |
| `VOICEPI_LOCAL_ONLY` | _(unset)_ | `1` → block cloud/BYOK backends and force model libraries offline; local models must already be downloaded |
| `VOICEPI_POST_PROCESSOR` | `none` | `ollama` or `openai` → run optional second text pass after STT/dictionary |
| `VOICEPI_POST_MODE` | `raw` | `clean`, `prompt`, `terminal`, `slack`, `email`, `bullets`; `bullet-list` is accepted as an alias; `raw` keeps current behavior |
| `VOICEPI_POST_MODEL` | `qwen2.5:3b` | local Ollama model or OpenAI-compatible chat model |
| `VOICEPI_POST_BASE_URL` | `http://localhost:11434` / `https://api.openai.com/v1` | Ollama or OpenAI-compatible chat endpoint |
| `VOICEPI_POST_API_KEY` | _(unset)_ | optional external post-processing key from env only; `OPENAI_API_KEY` also works |
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

Dictionary helper commands run before Whisper loads, for example
`whisper-dictate run --dictionary-status`,
`whisper-dictate run --dictionary-open`,
`whisper-dictate run --dictionary-add "Claude Code"`, and
`whisper-dictate run --dictionary-replace "Cloud Code=Claude Code"`. Use
`whisper-dictate run --dictionary-suggest benchmark\results.jsonl` to inspect benchmark
or history JSONL and print suggested smart replacements without mutating the
dictionary. In the Windows Settings UI, open the Dictionary tab and use
Benchmark suggestions to preview the same suggestions and apply the shown
replacement candidates to the configured dictionary.

Optional Parakeet backend: install `requirements-parakeet.txt`, then set
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
`whisper-large-v3-turbo`/`whisper-large-v3` for Groq. For external text cleanup, set
`VOICEPI_POST_PROCESSOR=openai`, `VOICEPI_POST_MODEL=<chat-model>` and
`OPENAI_API_KEY` or `VOICEPI_POST_API_KEY`. `VOICEPI_LOCAL_ONLY=1` blocks these
external providers before requests are made.

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

STT API keys saved from the Rust UI are stored in the OS credential store and
passed only to the managed Python worker process as `VOICEPI_STT_API_KEY`.
They are not written to `config.json` or shown in runtime command logs.
Terminal runs can still read keys from the process/user environment
(`OPENAI_API_KEY`, `GROQ_API_KEY`, `VOICEPI_STT_API_KEY`). Post-processing
cloud keys are still environment-only (`VOICEPI_POST_API_KEY` or
`OPENAI_API_KEY`).

File transcription for benchmarks/debugging uses the same backend, dictionary
and replacement pipeline as live dictation:

```powershell
python voice_pi.py --transcribe-file sample.wav
python voice_pi.py --transcribe-file sample.wav --json
python voice_pi.py --benchmark-files sample.wav `
  --benchmark-backends "whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3" `
  --benchmark-jsonl benchmark.jsonl
python voice_pi.py --benchmark-corpus benchmark\corpus.json `
  --benchmark-backends "whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3" `
  --benchmark-jsonl benchmark\results.jsonl
python voice_pi.py --calibrate-mic 5
python voice_pi.py --calibrate-file sample.wav --json
python voice_pi.py --history-last
python voice_pi.py --history-copy-last
python voice_pi.py --dictionary-suggest benchmark\results.jsonl --json
```

16-bit WAV works without extra tools. Other formats such as mp3/m4a require
`ffmpeg` on `PATH`. Benchmark mode runs each backend/model spec in an isolated
child process and writes one JSONL result per file/backend, including failures.
The corpus manifest in `benchmark/corpus.json` adds reference text, WER/CER and
expected technical-term hits/misses. Record missing local corpus audio with
`py -3.12 scripts\record-corpus.py --manifest benchmark\corpus.json --seconds 7`.
Calibration prints raw dBFS, noise floor, SNR, peak and recommended audio
threshold settings without loading an STT model.
`python voice_pi.py --model-capacity` prints NVIDIA GPU free/total VRAM and
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
elsewhere.

Spoken formatting commands are deterministic and opt-in. Set
`VOICEPI_FORMAT_COMMANDS=en`, `da`, or `both` globally or in a target profile.
Examples such as `first item comma new line second item period` or `første
punkt komma ny linje andet punkt punktum` become formatted punctuation and
line breaks. The feature is off by default so literal dictation of words like
`comma` or `punktum` is not changed unless you explicitly enable it.

Desktop UI: on Windows, use the Start-menu **whisper-dictate** shortcut. On
Linux, install the Rust UI with `scripts/install-linux-rust-ui.sh` and run
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

This triggers **`release.yml`**: it publishes the Linux bundle and Rust UI
binary, then builds the unified Windows installer and portable Windows ZIP bundle on a
Windows runner. It also publishes the GitHub Release and (when the
`HOMEBREW_TAP_TOKEN` repo secret is set)
auto-bumps `url`/`sha256` in
[`FactusConsulting/homebrew-tap`](https://github.com/FactusConsulting/homebrew-tap)
`Formula/whisper-dictate.rb`.

The Windows `.exe` installer and Windows ZIP bundle are built by the release
workflow itself. After the tag is pushed, CI uploads them to the Release and
regenerates the local winget manifests in this repo (used for the
`winget install --manifest` install path).

For a faster local Windows test loop without creating a release:

```powershell
.\scripts\build-windows-installer.ps1 -Version 0.0.0.1
```

The local installer and portable ZIP are written to `Output\`. The script uses
Inno Setup 6 and installs it via winget, or Chocolatey as a fallback, when it is
missing. Use a numeric four-part version for local builds because the Windows
installer metadata rejects labels such as `-local`.

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
