# Configuration reference

Every setting whisper-dictate reads, its possible values and defaults, and
how to set it on each platform. Two layers:

- **Environment variables** — read once at startup. Best when you launch
  from a Start-menu shortcut / installed launcher (no place to pass flags).
- **CLI flags** — passed to the launcher; override the matching env var for
  that run.

**Precedence:** a CLI flag wins over its env var (the flag's *default* is the
env var). `--autodetect` overrides `--lang`/`VOICEPI_LANG`. Settings persist
across upgrades only if they live **outside** the install dir (env vars, your
own shortcut) — never edit the installed `setup.*`/`voice_pi.py`, a clean
upgrade wipes them.

## Environment variables

| Variable | Default | Values | Effect |
|---|---|---|---|
| `VOICEPI_MODEL` | `large-v3-turbo` | any faster-whisper model: `large-v3-turbo`, `large-v3`, `medium`, `small`, `base`, `tiny`, `distil-large-v3` … | Whisper model. `large-v3-turbo` = fastest (default); `large-v3` = best accuracy, slower. Also `--model`. |
| `VOICEPI_DEVICE` | `auto` | `auto` \| `cuda` \| `cpu` | Compute device. `auto` = NVIDIA GPU if present, else CPU. Invalid value → error. Also `--device`. |
| `VOICEPI_COMPUTE_TYPE` | *(unset → `int8_float16` on GPU, `int8` on CPU)* | `int8` \| `int8_float16` \| `float16` \| `bfloat16` \| `float32` … (any ctranslate2-supported type) | Overrides the auto-picked compute precision. Big-GPU users gain accuracy with `float16` (or `bfloat16` on Ampere/Ada+); `int8_float16` defaults trade a little accuracy for VRAM/speed. Validated by ctranslate2 at model-load — an unsupported value raises then. Env only — no flag. |
| `VOICEPI_LANG` | *(unset → auto-detect)* | ISO 639-1: `da en de fr sv nb nn nl fi pl pt es it uk` … (any Whisper language); empty/unset = auto-detect | Force the spoken language. Strongly recommended for short/soft dictation — auto-detect flip-flops on short utterances. Also `--lang`. |
| `VOICEPI_BEAM_SIZE` | `1` | integer ≥ 1 (typical `1`–`5`) | Beam-search width. `1` = fastest; `5` = better accuracy, 3–4× slower on CPU (cheap on GPU). Env only — no flag. |
| `VOICEPI_INITIAL_PROMPT` | *(none)* | free text | Context/vocabulary hint biasing recognition toward your terms/names. Env only. |
| `VOICEPI_QUIT_COUNT` | `3` | integer ≥ 0 | **Windows/X11 only** (pynput path). N consecutive Esc presses within `VOICEPI_QUIT_WINDOW_MS` quit the app. Default `3` avoids accidental shutdown since pynput catches Esc system-wide. Set `0` to disable global Esc-quit entirely (rely on Ctrl+C in the launcher console); set `1` for legacy single-Esc behaviour. |
| `VOICEPI_QUIT_WINDOW_MS` | `1500` | integer ms | Time window within which the consecutive Esc presses count toward `VOICEPI_QUIT_COUNT`. Any non-Esc key press resets the counter. |
| `VOICEPI_TARGET_DBFS` | `-20` | float (dBFS, ≤ 0) | Loudness quiet input is normalised toward. Lower (e.g. `-16`) = boost quiet speech harder. |
| `VOICEPI_MIN_INPUT_DBFS` | `-55` | float (dBFS) | Reject utterances quieter than this ("input too quiet"). |
| `VOICEPI_MIN_SNR_DB` | `6` | float (dB) | Reject utterances with SNR below this ("no speech contrast"). |
| `VOICEPI_XKB_LAYOUT` | *(unset)* | XKB layout name: `dk se de fi no es pt br pl ua` … | **Wayland only.** Force the keycode layout for special-char injection, overriding auto-detection (highest priority). |
| `XKB_DEFAULT_LAYOUT` | *(unset)* | XKB layout name | **Wayland only.** Also consulted (2nd priority, after `VOICEPI_XKB_LAYOUT`). `--lang` auto-sets it if unset. |
| `VOICEPI_SKIP_SYSCHECK` | *(unset)* | any non-empty value | Linux: skip the `setup.sh` apt dependency check. Set automatically by the Homebrew/Nix wrappers; rarely set by hand. |

See [MICROPHONE.md](MICROPHONE.md) for what the capture-tuning dBFS/SNR
numbers mean in practice.

## CLI flags

Passed after the launcher (`setup.cmd` / `setup.sh` / `whisper-dictate`):

| Flag | Default | Values | Effect |
|---|---|---|---|
| `--key` | `ctrl_r` | pynput key name, or chord `a+b` | Hold-to-talk key. e.g. `ctrl_r`, `alt_r`, `shift_r`, `f9`, or `shift_r+ctrl_r` (hold both). |
| `--model NAME` | `$VOICEPI_MODEL` | see `VOICEPI_MODEL` | Whisper model for this run. |
| `--lang CODE` | `$VOICEPI_LANG` | ISO 639-1 code | Force language for this run. Omit to auto-detect. |
| `--autodetect` | off | — | Force language auto-detect (overrides `--lang`/`VOICEPI_LANG`). |
| `--device D` | `$VOICEPI_DEVICE` | `auto` \| `cuda` \| `cpu` | Compute device for this run. |
| `--paste` | off | — | X11/Windows: inject via clipboard + Ctrl+V. (Wayland always uses direct evdev keycodes regardless.) |
| `--no-type` | off | — | Print the transcription only, don't inject (testing). |

## How to set them, per environment

### Windows (.exe installer)

The Start-menu shortcut runs the launcher with **no arguments**, so env vars
are the way to configure it persistently:

```powershell
# Persistent (survives upgrades; honoured by the Start-menu shortcut).
setx VOICEPI_LANG da
setx VOICEPI_BEAM_SIZE 5
setx VOICEPI_INITIAL_PROMPT "rødgrød med fløde, FactusConsulting, whisper-dictate"
setx VOICEPI_MODEL large-v3
setx VOICEPI_DEVICE cuda
# then restart whisper-dictate (new process picks them up)
```

One-off via terminal (the installer put the dir on PATH):

```powershell
& "$env:LOCALAPPDATA\Programs\WhisperDictate\setup.cmd" --key ctrl_r --lang da --model large-v3 --device cuda
```

Or make your **own** shortcut whose Target is
`%LOCALAPPDATA%\Programs\WhisperDictate\setup.cmd --key ctrl_r --lang da`
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

## Quick recommendations

- **Daily Danish dictation:** `VOICEPI_LANG=da` (persistent). Add
  `VOICEPI_INITIAL_PROMPT` with your domain terms.
- **GPU desktop, max quality:** `--device cuda --model large-v3` +
  `VOICEPI_BEAM_SIZE=5` + `VOICEPI_COMPUTE_TYPE=float16` (full half-precision
  instead of the quantised `int8_float16` default; latency is cheap on GPU).
- **Multilingual:** leave `VOICEPI_LANG` unset (auto-detect) — but speak full,
  clear sentences; auto-detect is unreliable on short utterances.
- **Mic too quiet / noisy:** see [MICROPHONE.md](MICROPHONE.md) before tuning
  `VOICEPI_TARGET_DBFS`/`VOICEPI_MIN_*`.
