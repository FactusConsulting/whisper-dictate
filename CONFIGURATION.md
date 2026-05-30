# Configuration reference

Every setting whisper-dictate reads, its possible values and defaults, and
how to set it on each platform. Two layers:

- **Environment variables** — read once at startup. Best when you launch
  from a Start-menu shortcut / installed launcher (no place to pass flags).
- **CLI flags** — passed to the launcher; override the matching env var for
  that run.

**Precedence:** a CLI flag wins over its env var/config default for that run.
`--autodetect` overrides `--lang`/`VOICEPI_LANG`. Settings persist across
upgrades only if they live **outside** the install dir (env vars,
`%APPDATA%\WhisperDictate\config.json`, your own shortcut) — never edit the
installed `setup.*`/`voice_pi.py`, a clean upgrade wipes them.

`config.json` is read before env-var fallback for every matching setting, so
the settings UI can own your normal defaults while old `setx VOICEPI_*` values
still work for keys that are absent from the JSON file.

## Cheat sheet — every knob at a glance

| Knob | Env var | CLI flag | Default | Range / options | What it does |
|---|---|---|---|---|---|
| **Whisper model** | `VOICEPI_MODEL` | `--model` | `large-v3-turbo` | `large-v3-turbo`, `large-v3`, `medium`, `small`, `base`, `tiny`, `distil-large-v3`, … | turbo = fastest default; `large-v3` = best accuracy |
| **STT backend** | `VOICEPI_STT_BACKEND` | _none_ | `whisper` | `whisper` \| `parakeet` | default uses faster-whisper; `parakeet` uses optional NVIDIA NeMo dependencies |
| **Device** | `VOICEPI_DEVICE` | `--device` | `auto` | `auto` \| `cuda` \| `cpu` | auto picks NVIDIA GPU if present, else CPU |
| **Compute type / precision** | `VOICEPI_COMPUTE_TYPE` | _none_ | `int8_float16` (GPU) / `int8` (CPU) | `int8`, `int8_float16`, `float16`, `bfloat16`, `float32` | precision override — `float16` for accuracy on big GPUs; see VRAM table below |
| **Spoken language** | `VOICEPI_LANG` | `--lang` / `--autodetect` | _(unset → auto-detect)_ | ISO 639-1: `da`, `en`, `de`, `fr`, `sv`, `nb`, `nl`, `fi`, `pl`, `pt`, `es`, `it`, `uk`, … | language hint; strongly recommended for short utterances |
| **Beam-search width** | `VOICEPI_BEAM_SIZE` | _none_ | `1` | integer ≥ 1 (typical 1-16) | wider = more accurate, slower (cheap on GPU) |
| **Decode temperatures** | `VOICEPI_TEMPERATURE` | _none_ | `0.0,0.2` | CSV floats (e.g. `0.0`, `0.0,0.2,0.4`) | Whisper's fallback ladder. `0.0` locks to greedy decode = predictable output, no "creative" fallback on uncertainty. |
| **Context for long ytringer** | `VOICEPI_CONTEXT_MIN_SECONDS` | _none_ | `0` (off) | float seconds (`0` = disabled, `5` = enable for utterances ≥ 5 s) | Pass `condition_on_previous_text=True` only when an utterance is at least this long. Helps Whisper keep word boundaries on long sentences without triggering hallucinations on short ones. |
| **Vocabulary hint** | `VOICEPI_INITIAL_PROMPT` | _none_ | _(unset)_ | free text up to ~1024 chars | bias toward your domain words/names |
| **Custom dictionary** | `VOICEPI_DICTIONARY` | _none_ | user config path | JSON/text file path(s) | bounded vocabulary prompt + exact smart replacements for names like `Claude Code`, `Codex`, `OpenClaw` |
| **Push-to-talk key** | `VOICEPI_KEY` | `--key` | `ctrl_r` | pynput key name (`ctrl_r`, `alt_r`, `f9`, …) or `a+b` chord | hold-to-talk key |
| **Inject mode** | `VOICEPI_INJECT_MODE` | `--type` / `--paste` / `--no-type` | `auto` | `auto` \| `type` \| `paste` \| `print` | auto-select injection strategy, force typing, force clipboard paste (X11/Win), or print-only |
| **Global quit count** | `VOICEPI_QUIT_COUNT` | _none_ | `3` | integer ≥ 0 (`0` disables) | N consecutive Esc to quit (Windows/X11) |
| **Quit window** | `VOICEPI_QUIT_WINDOW_MS` | _none_ | `1500` | integer ms | time window for the consecutive Esc presses |
| **Audio loudness target** | `VOICEPI_TARGET_DBFS` | _none_ | `-20` | float dBFS ≤ 0 | target for quiet-boost normalisation |
| **Audio min input** | `VOICEPI_MIN_INPUT_DBFS` | _none_ | `-55` | float dBFS | reject input quieter than this |
| **Audio min SNR** | `VOICEPI_MIN_SNR_DB` | _none_ | `6` | float dB | reject input below this speech-vs-noise contrast |
| **XKB layout (Wayland)** | `VOICEPI_XKB_LAYOUT` (highest), `XKB_DEFAULT_LAYOUT` (fallback) | _none_ | _(auto-detect)_ | `dk`, `se`, `de`, `fi`, `no`, `es`, `pt`, `br`, `pl`, `ua`, … | force keycode layout for special-char injection |
| **JSON output** | `VOICEPI_JSON` | `--json` | _(unset)_ | truthy / falsey | print one structured JSON event per accepted utterance |
| **Metrics file** | `VOICEPI_METRICS_JSONL` | _none_ | _(unset)_ | file path | append one structured JSON event per accepted utterance |
| **STT segment debug** | `VOICEPI_STT_DEBUG` | _none_ | _(unset)_ | truthy / falsey | print Whisper segment metadata (`avg_logprob`, `no_speech_prob`, `compression_ratio` when available) |
| **VAD threshold** | `VOICEPI_VAD_THRESHOLD` | _none_ | `0.3` | float | Silero VAD speech threshold passed to faster-whisper |
| **VAD silence** | `VOICEPI_VAD_MIN_SILENCE_MS` | _none_ | `600` | integer ms | minimum silence gap used by VAD segmentation |
| **Skip syscheck** | `VOICEPI_SKIP_SYSCHECK` | _none_ | _(unset)_ | any non-empty | skip `setup.sh` apt-dep check (auto-set by brew/nix) |
| **Debug dump** | `VOICEPI_DEBUG` | _none_ | _(unset)_ | `1` / `true` / any truthy | log every effective setting at startup |

The detailed tables below are the same knobs split by surface (env vars
vs flags) with the longer prose. Most users only need the cheat sheet +
the **GPU VRAM sizing** table further down.

## Environment variables

| Variable | Default | Values | Effect |
|---|---|---|---|
| `VOICEPI_MODEL` | `large-v3-turbo` | any faster-whisper model: `large-v3-turbo`, `large-v3`, `medium`, `small`, `base`, `tiny`, `distil-large-v3` … | Whisper model. `large-v3-turbo` = fastest (default); `large-v3` = best accuracy, slower. Also `--model`. |
| `VOICEPI_STT_BACKEND` | `whisper` | `whisper` \| `parakeet` | Selects the local STT engine. `parakeet` loads NVIDIA NeMo lazily and uses `nvidia/parakeet-tdt-0.6b-v3` when the normal Whisper default model is unchanged. |
| `VOICEPI_PARAKEET_MODEL` | `nvidia/parakeet-tdt-0.6b-v3` | NeMo ASR model name | Optional Parakeet-specific model override. Takes precedence over `VOICEPI_MODEL` when `VOICEPI_STT_BACKEND=parakeet`. |
| `VOICEPI_DEVICE` | `auto` | `auto` \| `cuda` \| `cpu` | Compute device. `auto` = NVIDIA GPU if present, else CPU. Invalid value → error. Also `--device`. |
| `VOICEPI_COMPUTE_TYPE` | *(unset → `int8_float16` on GPU, `int8` on CPU)* | `int8` \| `int8_float16` \| `float16` \| `bfloat16` \| `float32` … (any ctranslate2-supported type) | Overrides the auto-picked compute precision. Big-GPU users gain accuracy with `float16` (or `bfloat16` on Ampere/Ada+); `int8_float16` defaults trade a little accuracy for VRAM/speed. Validated by ctranslate2 at model-load — an unsupported value raises then. Env only — no flag. |
| `VOICEPI_LANG` | *(unset → auto-detect)* | ISO 639-1: `da en de fr sv nb nn nl fi pl pt es it uk` … (any Whisper language); empty/unset = auto-detect | Force the spoken language. Strongly recommended for short/soft dictation — auto-detect flip-flops on short utterances. Also `--lang`. |
| `VOICEPI_KEY` | `ctrl_r` | pynput key name, or chord `a+b` | Hold-to-talk key. e.g. `ctrl_r`, `alt_r`, `shift_r`, `f9`, or `shift_r+ctrl_r` (hold both). Also `--key`. |
| `VOICEPI_BEAM_SIZE` | `1` | integer ≥ 1 (typical `1`–`5`) | Beam-search width. `1` = fastest; `5` = better accuracy, 3–4× slower on CPU (cheap on GPU). Env only — no flag. |
| `VOICEPI_INITIAL_PROMPT` | *(none)* | free text | Context/vocabulary hint biasing recognition toward your terms/names. Env only. |
| `VOICEPI_DICTIONARY` | platform user config path | path list (`;` on Windows, `:` on Unix) | Load one or more custom dictionaries. JSON supports `terms` and `replacements`; text files support `[terms]` / `[replacements]`. Terms are appended to the Whisper prompt within the configured limits; replacements run after transcription. Env only. |
| `VOICEPI_DICTIONARY_ENABLED` | `1` | truthy / falsey | Set `0`, `false`, `no`, or `off` to disable dictionary loading without removing the file. |
| `VOICEPI_DICTIONARY_MAX_TERMS` | `80` | integer ≥ 0 | Maximum number of dictionary terms appended to the prompt. Keeps prompt injection bounded as the dictionary grows. |
| `VOICEPI_DICTIONARY_PROMPT_CHARS` | `1200` | integer ≥ 0 | Maximum total characters used by dictionary terms in the prompt. |
| `VOICEPI_INJECT_MODE` | `auto` | `auto` \| `type` \| `paste` \| `print` | Controls text output injection. `auto` types directly except for known fragile Windows terminal targets, where it uses clipboard paste. `type` always sends direct keystrokes, `paste` copies the text to the clipboard and sends paste on X11/Windows, and `print` only writes the transcription to stdout. `--type`/`--paste`/`--no-type` override this env var. |
| `VOICEPI_QUIT_COUNT` | `3` | integer ≥ 0 | **Windows/X11 only** (pynput path). N consecutive Esc presses within `VOICEPI_QUIT_WINDOW_MS` quit the app. Default `3` avoids accidental shutdown since pynput catches Esc system-wide. Set `0` to disable global Esc-quit entirely (rely on Ctrl+C in the launcher console); set `1` for legacy single-Esc behaviour. |
| `VOICEPI_QUIT_WINDOW_MS` | `1500` | integer ms | Time window within which the consecutive Esc presses count toward `VOICEPI_QUIT_COUNT`. Any non-Esc key press resets the counter. |
| `VOICEPI_TARGET_DBFS` | `-20` | float (dBFS, ≤ 0) | Loudness quiet input is normalised toward. Lower (e.g. `-16`) = boost quiet speech harder. |
| `VOICEPI_MIN_INPUT_DBFS` | `-55` | float (dBFS) | Reject utterances quieter than this ("input too quiet"). |
| `VOICEPI_MIN_SNR_DB` | `6` | float (dB) | Reject utterances with SNR below this ("no speech contrast"). |
| `VOICEPI_XKB_LAYOUT` | *(unset)* | XKB layout name: `dk se de fi no es pt br pl ua` … | **Wayland only.** Force the keycode layout for special-char injection, overriding auto-detection (highest priority). |
| `XKB_DEFAULT_LAYOUT` | *(unset)* | XKB layout name | **Wayland only.** Also consulted (2nd priority, after `VOICEPI_XKB_LAYOUT`). `--lang` auto-sets it if unset. |
| `VOICEPI_JSON` | *(unset)* | `1` / `true` / any truthy | Print one structured JSON event per accepted utterance to stdout. Also `--json`. |
| `VOICEPI_METRICS_JSONL` | *(unset)* | file path | Append one JSON object per accepted utterance. Includes recording duration, STT compute time, real-time factor, model/device, injection strategy, target title/process, language confidence, text preview, and segment metadata. |
| `VOICEPI_STT_DEBUG` | *(unset)* | `1` / `true` / any truthy | Print per-segment Whisper metadata when available. Useful for diagnosing hallucinations and low-confidence output. |
| `VOICEPI_VAD_THRESHOLD` | `0.3` | float | Silero VAD speech threshold passed to faster-whisper. Higher rejects more non-speech but can clip quiet speech. |
| `VOICEPI_VAD_MIN_SILENCE_MS` | `600` | integer ms | Minimum silence gap used by VAD segmentation. Lower can reduce latency on clipped phrases; higher keeps phrases together. |
| `VOICEPI_SKIP_SYSCHECK` | *(unset)* | any non-empty value | Linux: skip the `setup.sh` apt dependency check. Set automatically by the Homebrew/Nix wrappers; rarely set by hand. |
| `VOICEPI_DEBUG` | *(unset)* | `1` / `true` / any truthy (empty, `0`, `false`, `no`, `off` = disabled) | At startup, prints a `[debug] effective settings:` block listing every setting + which env var supplied it. Useful for "is my `setx` actually arriving in the running process?" — run with `VOICEPI_DEBUG=1` and the first lines of the log show the truth. Zero runtime cost when unset. |

See [MICROPHONE.md](MICROPHONE.md) for what the capture-tuning dBFS/SNR
numbers mean in practice.

### Probing a hotkey before you commit — `scripts/probe-key.py`

Before `setx VOICEPI_KEY <something>`, verify your OS actually delivers
that key to pynput. The repo ships a 100-line standalone probe:

```powershell
# Clone or cd into the repo, then:
python scripts/probe-key.py pause          # active: confirm Pause arrives
python scripts/probe-key.py ctrl_r+space   # active: confirm a chord
python scripts/probe-key.py                # passive: log EVERY key event
python scripts/probe-key.py f9 30          # custom 30-second window
```

Common gotchas the probe catches:

- **Pause/Break missing on tenkeyless / laptop keyboards** — no physical
  Pause key, nothing to trigger.
- **Pause intercepted by gaming-keyboard firmware** (Razer/Corsair) —
  swallowed before pynput sees it.
- **`caps_lock` state-toggle on Windows** — press fires once, release
  doesn't fire on hold; breaks the hold-to-talk model.
- **Multimedia keys eaten by OEM software** before reaching pynput.
- **Chord like `ctrl_r+space` filtered by IME / IntelliSense** in some
  apps.

Exit codes: `0` = chord verified, `1` = no events at all (OS not
delivering), `2` = events arrived but the full chord was never held
together, `3` = unknown key name. The script needs no install beyond
pynput (which whisper-dictate already depends on).

### Debugging "is my `setx` arriving?" — `VOICEPI_DEBUG=1`

A common confusion on Windows is that `setx` writes to the user registry,
but **only new processes inherit it** — a whisper-dictate launched from a
stale Start-menu shortcut or tray-restart may still see the old values.

To verify what the running process actually sees, set `VOICEPI_DEBUG=1`
and restart. The first lines of the log will print every effective
setting + the env var that supplied it:

```
[debug] effective settings:
  --key              ctrl_r
  --model            large-v3  (env VOICEPI_MODEL=large-v3)
  --lang             da  (env VOICEPI_LANG=da, --autodetect=False)
  --device           cuda  ->  resolved: cuda / float16
  stt backend        whisper  (env VOICEPI_STT_BACKEND=(unset))
  compute_type       float16  (env VOICEPI_COMPUTE_TYPE=float16)
  beam_size          8  (env VOICEPI_BEAM_SIZE=8)
  initial_prompt     899 chars: "Factus Consulting, TwoDay, Hetzner, konsulent..."  (env VOICEPI_INITIAL_PROMPT)
  dictionary         14 terms, 5 replacements, path=C:\Users\me\AppData\Roaming\WhisperDictate\dictionary.json
  quit               3x Esc within 1500ms  (env VOICEPI_QUIT_COUNT=3)
  audio thresholds   target_dbfs=-20.0  min_input_dbfs=-55.0  min_snr_db=6.0
  XKB (Wayland)      VOICEPI_XKB_LAYOUT=(unset)  XKB_DEFAULT_LAYOUT=da
  inject mode        auto  (env VOICEPI_INJECT_MODE=(unset))
loading Whisper large-v3 on cuda (float16)…
```

If a value shows `(unset)` where you expected one, your `setx` didn't
reach this process — log out + back in, or launch from a fresh PowerShell
where `$env:VOICEPI_X` shows the value. Leave `VOICEPI_DEBUG` unset for
normal use; the dump adds ~10 lines on startup and zero runtime cost.

## CLI flags

Passed after the launcher (`setup.cmd` / `setup.sh` / `whisper-dictate`):

| Flag | Default | Values | Effect |
|---|---|---|---|
| `--key` | `$VOICEPI_KEY` or `ctrl_r` | pynput key name, or chord `a+b` | Hold-to-talk key. e.g. `ctrl_r`, `alt_r`, `shift_r`, `f9`, or `shift_r+ctrl_r` (hold both). |
| `--model NAME` | `$VOICEPI_MODEL` | see `VOICEPI_MODEL` | Whisper model for this run. |
| `--lang CODE` | `$VOICEPI_LANG` | ISO 639-1 code | Force language for this run. Omit to auto-detect. |
| `--autodetect` | off | — | Force language auto-detect (overrides `--lang`/`VOICEPI_LANG`). |
| `--device D` | `$VOICEPI_DEVICE` | `auto` \| `cuda` \| `cpu` | Compute device for this run. |
| `--type` | `$VOICEPI_INJECT_MODE` or off | — | Force direct keyboard typing on X11/Windows. (Wayland always uses direct evdev keycodes regardless.) |
| `--paste` | `$VOICEPI_INJECT_MODE` or off | — | Force clipboard + Ctrl+V on X11/Windows. (Wayland always uses direct evdev keycodes regardless.) |
| `--no-type` | `$VOICEPI_INJECT_MODE` or off | — | Print the transcription only, don't inject (testing). |
| `--json` | `$VOICEPI_JSON` or off | — | Also print one structured JSON event per accepted utterance. |
| `--doctor` | off | — | Run Linux/Wayland health checks and exit before loading Whisper. |

## How to set them, per environment

### Windows (.exe installer)

The Start-menu shortcut runs the launcher with **no arguments**, so env vars
are the way to configure it persistently:

```powershell
# Persistent (survives upgrades; honoured by the Start-menu shortcut).
setx VOICEPI_LANG da
setx VOICEPI_BEAM_SIZE 5
setx VOICEPI_INITIAL_PROMPT "rødgrød med fløde, FactusConsulting, whisper-dictate"
setx VOICEPI_DICTIONARY "%APPDATA%\WhisperDictate\dictionary.json"
setx VOICEPI_MODEL large-v3
setx VOICEPI_DEVICE cuda
setx VOICEPI_KEY "ctrl_l+space"
setx VOICEPI_INJECT_MODE auto
# then restart whisper-dictate (new process picks them up)
```

One-off via terminal (the installer put the dir on PATH):

```powershell
& "$env:LOCALAPPDATA\Programs\WhisperDictate\setup.cmd" --key ctrl_r --lang da --model large-v3 --device cuda
```

Or make your **own** shortcut whose Target is
`%LOCALAPPDATA%\Programs\WhisperDictate\setup.cmd --key ctrl_r --lang da`

### Optional NVIDIA Parakeet backend

The default backend remains faster-whisper. To try NVIDIA Parakeet, install the
normal requirements first, then the optional NeMo requirements. This path is
experimental on Windows because NeMo/PyTorch wheel compatibility depends on the
local CUDA/Python combination:

```powershell
& "$env:USERPROFILE\voice-pi-venv\Scripts\python.exe" -m pip install `
  -r "$env:LOCALAPPDATA\Programs\WhisperDictate\requirements-parakeet.txt"
setx VOICEPI_STT_BACKEND parakeet
```

`VOICEPI_STT_BACKEND=parakeet` loads NeMo only when transcription starts, so
`--help`, `--doctor`, and the default Whisper backend do not require Parakeet
dependencies. With the default `VOICEPI_MODEL=large-v3-turbo`, the adapter uses
`nvidia/parakeet-tdt-0.6b-v3`; set `VOICEPI_PARAKEET_MODEL` or `--model` to use
another NeMo ASR model. v3 is multilingual and includes Danish, which makes it
the right first Parakeet candidate for mixed Danish/English dictation.

### Optional PySide/Qt settings UI

Install the optional UI dependency into the same venv, then open the settings
window. On Windows, the installer adds a Start-menu **whisper-dictate**
shortcut which opens a unified control UI: it starts/stops dictation, shows the
runtime log, saves settings and restarts dictation automatically when a
restart-only setting changes. On Linux/macOS the UI remains settings-only; keep
the normal terminal dictation process for now.

```powershell
& "$env:USERPROFILE\voice-pi-venv\Scripts\python.exe" -m pip install `
  -r "$env:LOCALAPPDATA\Programs\WhisperDictate\requirements-ui.txt"
& "$env:LOCALAPPDATA\Programs\WhisperDictate\setup.ps1" --settings-ui
```

The UI edits `%APPDATA%\WhisperDictate\config.json`, keeps a tray icon alive,
can open the dictionary file, and writes a reload signal after saving. A
running dictation process applies live-safe changes on the next record
start/stop: language, inject mode, dictionary, VAD, audio thresholds, prompt,
JSON/metrics and debug flags. Backend, model, device, compute type and hotkey
are restart-only; the Windows control UI restarts its managed dictation process
for those changes.

To signal a manual reload without the UI:

```powershell
Set-Content "$env:APPDATA\WhisperDictate\config.reload" `
  ([DateTimeOffset]::Now.ToUnixTimeMilliseconds())
```

### Custom dictionary

Use a dictionary when product names, app names and mixed-language terms are
too numerous for one long `VOICEPI_INITIAL_PROMPT`. On Windows, the default
path is `%APPDATA%\WhisperDictate\dictionary.json`; on Linux/macOS it is
`${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/dictionary.json`. You can also
point `VOICEPI_DICTIONARY` at one or more files.

```json
{
  "terms": ["Slack", "Claude Code", "Codex", "OpenClaw", "GitHub Actions"],
  "replacements": {
    "Cloud Code": "Claude Code",
    "code X": "Codex",
    "open claw": "OpenClaw"
  }
}
```

Dictionary terms bias Whisper through a bounded prompt
(`VOICEPI_DICTIONARY_MAX_TERMS`, `VOICEPI_DICTIONARY_PROMPT_CHARS`). Smart
replacements run after transcription and are recorded in JSON/metrics output
along with the raw text.

Manage the default dictionary without loading Whisper:

```powershell
& "$env:LOCALAPPDATA\Programs\WhisperDictate\setup.ps1" --dictionary-status
& "$env:LOCALAPPDATA\Programs\WhisperDictate\setup.ps1" --dictionary-open
& "$env:LOCALAPPDATA\Programs\WhisperDictate\setup.ps1" --dictionary-add "Claude Code"
& "$env:LOCALAPPDATA\Programs\WhisperDictate\setup.ps1" --dictionary-replace "Cloud Code=Claude Code"
```

### Injection smoke test

To test a target app without loading Whisper, focus the input field and run:

```powershell
python scripts/inject-smoke.py --mode auto
python scripts/inject-smoke.py --mode type
python scripts/inject-smoke.py --mode paste
```

Use this to compare Notepad, Windows Terminal, Claude Code, browser text
areas, and other targets with the exact same injection code path as the app.

## Version display

The launcher prints `whisper-dictate <version>` when the terminal window opens.
Release zips and Windows installers include a `VERSION` file generated from
the release tag; development checkouts fall back to `git describe`.
(don't edit the installer-created shortcut — an upgrade may recreate it).

Revert language to auto: `setx VOICEPI_LANG ""` then restart, or pass
`--autodetect`.

### Linux — Homebrew

The `whisper-dictate` command is on PATH. Persist env in `~/.profile` /
`~/.bashrc`:

```bash
echo 'export VOICEPI_LANG=da'        >> ~/.profile
echo 'export VOICEPI_BEAM_SIZE=5'    >> ~/.profile
# new shell, then:
whisper-dictate --key shift_r+ctrl_r --lang da
```

Or inline for one run:

```bash
VOICEPI_LANG=da VOICEPI_BEAM_SIZE=5 whisper-dictate --key shift_r+ctrl_r
```

### Linux — manual (`./setup.sh`)

Same as Homebrew — env vars or flags:

```bash
VOICEPI_LANG=da ./setup.sh --key ctrl_r --lang da
```

### NixOS / Nix

`nix run` — env before the command, flags after `--`:

```bash
VOICEPI_LANG=da VOICEPI_BEAM_SIZE=5 \
  nix run github:FactusConsulting/whisper-dictate -- --key shift_r+ctrl_r --lang da
```

NixOS module — set env in the service/user environment (e.g.
`environment.sessionVariables.VOICEPI_LANG = "da";`) and the wrapper inherits
it. `VOICEPI_XKB_LAYOUT` is auto-derived from `--lang`/the session layout; the
module already wires up ydotool/uinput for Wayland.

## GPU VRAM sizing — what to set per card

Pick the row matching your **free** VRAM (run `nvidia-smi --query-gpu=memory.free
--format=csv` — browser/IDE/Discord eat 1–3 GB before whisper-dictate starts,
so free ≠ total). Round down to the nearest row. If the first transcription
OOMs, drop `BEAM_SIZE` one row or `COMPUTE_TYPE` one tier (`float16` →
`int8_float16`).

| Free VRAM | Device | Model | `BEAM_SIZE` | `COMPUTE_TYPE` | Footprint¹ | Notes |
|---|---|---|---:|---|---:|---|
| **CPU only / <2 GB** | `cpu` | `large-v3-turbo` | `1` | *(default `int8`)* | RAM, not VRAM | `beam>1` too slow on CPU; turbo beats large-v3 here |
| **2–4 GB** *(GTX 1660, mobile RTX 3050)* | `cuda` | `large-v3-turbo` | `1`–`5` | *(default `int8_float16`)* | ~1–1.5 GB | small footprint, near-large quality |
| **4–6 GB** *(RTX 3050 8 GB, mobile 4060)* | `cuda` | `large-v3` | `5` | *(default `int8_float16`)* | ~2.5–3 GB | quantised default keeps room for other apps |
| **6–8 GB** *(RTX 3060 8 GB, RTX 4060)* | `cuda` | `large-v3` | `5`–`8` | `float16` | ~3.5–4.5 GB | full half-precision; small accuracy win |
| **8–12 GB** *(RTX 3080 10 GB, RTX 4070)* | `cuda` | `large-v3` | `8` | `float16` | ~4–5 GB | sweet spot for desktop GPUs |
| **12–16 GB** *(RTX 3060 12 GB, RTX 4080, 5070 Ti)* | `cuda` | `large-v3` | `10` | `float16` *(or `bfloat16` on Ampere+)* | ~5–6 GB | wider beam helps on hard/short utterances |
| **16–24 GB** *(RTX 4080/5080 16 GB)* | `cuda` | `large-v3` | `10`–`16` | `float16` | ~6–8 GB | beam past 16 has diminishing returns |
| **24+ GB** *(RTX 3090/4090/5090, A40, A100, H100)* | `cuda` | `large-v3` | `16` | `float32` *(or stay on `float16`)* | ~10–12 GB | `float32` is overkill — Whisper accuracy plateaus before this |

¹ Footprint = model weights + KV cache (~25 MB per beam at ~30 s audio) +
ctranslate2/CUDA context (~300–500 MB). `large-v3` weights alone:
~1.6 GB `int8_float16`, ~3.1 GB `float16`/`bfloat16`, ~6.2 GB `float32`.
`large-v3-turbo` is roughly half of those.

**One-liner to set the 8–12 GB row** (RTX 3080 / 4070):
```powershell
setx VOICEPI_DEVICE cuda; setx VOICEPI_MODEL large-v3; setx VOICEPI_BEAM_SIZE 8; setx VOICEPI_COMPUTE_TYPE float16; setx VOICEPI_LANG da
# restart whisper-dictate; first [stt] line in the log will show your new compute type
```

## Quick recommendations

- **Daily Danish dictation:** `VOICEPI_LANG=da` (persistent). Add
  `VOICEPI_INITIAL_PROMPT` with your domain terms.
- **GPU desktop, max quality:** see the VRAM sizing table above — pick the row
  matching your free VRAM, not your total.
- **Multilingual:** leave `VOICEPI_LANG` unset (auto-detect) — but speak full,
  clear sentences; auto-detect is unreliable on short utterances.
- **Mic too quiet / noisy:** see [MICROPHONE.md](MICROPHONE.md) before tuning
  `VOICEPI_TARGET_DBFS`/`VOICEPI_MIN_*`.
