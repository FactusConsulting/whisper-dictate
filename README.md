# whisper-dictate — speak prompts instead of typing them

App-agnostic **dictation** for Windows. Hold a key, speak *quietly but
clearly*, release — the transcribed text is injected into whatever
window has focus: a terminal, an AI chat in the browser, an editor,
anything. Fully local: Whisper runs on your own NVIDIA GPU, no cloud
STT, nothing leaves the machine.

This is a **mic → keyboard**, not an AI chat. There is deliberately no
model/conversation logic — the "AI" (or text field) is whatever app
you're already in. Switching target = just focus a different window.

Everything runs **on Windows in one process** (`voice_pi.py`): mic
capture and Whisper inference together, no server, no network hop.

## Shape

```
🎤 hold key, speak softly
   │   (mic on Windows)
   ▼
voice_pi.py  ── one Windows Python process ───────────────────┐
   │  faster-whisper on your NVIDIA GPU (native CUDA)          │
   │  capture → boost quiet audio → transcribe → inject        │
   ▼                                                           │
injects text at your cursor ◄────── plain text ────────────────┘
   ▼
[ terminal / browser chat / editor — whatever's focused ]
```

Soft-but-voiced speech is the design target. What matters is the
capture/gate chain, not raw model size:

- **Quiet-audio gain** (`VOICEPI_TARGET_DBFS`, default −20): soft speech
  lands at −35..−45 dBFS where Whisper's no-speech gate eats it; the
  audio is boosted toward −20 without clipping before the model sees it.
- **VAD threshold 0.3** (Silero default 0.5) keeps soft voiced speech.
- Relaxed no-speech/log-prob gates + a temperature fallback so a quiet
  real utterance gets a second chance, not an empty string.
- Greedy decode (`beam_size=1`) — beam width is the dominant latency
  cost for short turns and buys little here; robustness is in the
  encoder + the gain/VAD chain, not beam width.
- A `[cap]` line prints captured loudness, applied gain, noise floor
  and **SNR** so you can tell on data whether your mic is the limit.
- A close-talk/headset mic beats a far-field laptop mic by a lot.

## Which download (release variants)

`device` auto-detects at runtime: NVIDIA GPU → CUDA, otherwise CPU
(slower — that's why the default model is the fastest, `large-v3-turbo`).
faster-whisper/ctranslate2 only accelerate on **NVIDIA**; an **AMD**
GPU has no usable acceleration here and runs the CPU build.

| Release asset | Use on | Engine |
|---|---|---|
| `whisper-dictate-windows-nvidia.zip` | Windows + NVIDIA GPU | CUDA (fast) |
| `whisper-dictate-windows-cpu.zip` | Windows, no NVIDIA (incl. AMD-GPU boxes) | CPU |
| `whisper-dictate-linux-cpu.zip` | Ubuntu 26.04 / 24.04, no NVIDIA | CPU |

Same code in all three; they differ only in the bundled requirements
file and launcher. Unzip, run the launcher, done.

## Requirements

- **Windows:** the launcher fetches official **CPython 3.12** if
  missing (via `winget`). 3.13/3.14 and MinGW/MSYS Python are rejected
  on purpose — the binary wheel stack (`ctranslate2`, `onnxruntime`,
  `nvidia-*-cu12`) ships MSVC wheels for 3.12.
- **Linux:** system `python3` ≥ 3.10, plus `libportaudio2` (mic) and a
  clipboard tool (`wl-clipboard` on Wayland, `xclip` on X11) — the
  launcher tells you the exact `apt` line if anything's missing.
- GPU build: ~2 GB free VRAM. Model on disk: turbo ~1.5 GB, large-v3
  ~3 GB (fetched once into the Hugging Face cache).

> **Linux + Wayland (important):** global hotkey capture and synthetic
> keystroke injection (pynput) are X11 features; GNOME/Wayland blocks
> both for unprivileged apps, so push-to-talk and auto-typing may not
> work out of the box. The venv, model and transcription work fine
> regardless. Realistic options: log in as **"Ubuntu on Xorg"**; or
> use `--no-type` to just see the transcription; or the proper Wayland
> path (evdev hotkey + `ydotool` injection, needs one-time
> `input`-group / uinput permissions) — a known follow-up, not yet
> bundled. This is a genuine platform limitation, not a config bug.

## Setup — one script, portable

Unzip the right variant (or copy the whole repo folder), then:

**Windows:**
```powershell
powershell -ExecutionPolicy Bypass -File setup.ps1
```
**Linux:**
```bash
./setup.sh
```

Idempotent and self-contained: first run finds/installs Python, builds
a **machine-local** venv (never inside the copied folder), installs
deps, downloads the model and launches; later runs just launch.
Nothing is hardcoded to a user or path.

Any arguments pass straight to `voice_pi.py`; with none it defaults to
`--paste` (model defaults to `large-v3-turbo`):

```powershell
powershell -ExecutionPolicy Bypass -File setup.ps1 --lang de        # German
powershell -ExecutionPolicy Bypass -File setup.ps1 --autodetect     # guess language
powershell -ExecutionPolicy Bypass -File setup.ps1 --device cpu     # force CPU
```

Manual setup (if you'd rather not use the launcher):

```bash
python3 -m venv ~/.venv-whisper-dictate                 # Windows: py -m venv ...
~/.venv-whisper-dictate/bin/pip install -r requirements-cpu.txt   # or -gpu.txt
~/.venv-whisper-dictate/bin/python voice_pi.py --paste
```

## Use

1. Start it (the script, or `python voice_pi.py`). Leave it running.
2. Click into the terminal / browser chat / editor where you want text.
3. **Hold Right Ctrl, speak your prompt softly, release.**
4. ~1–2 s later the text appears at your cursor. Press Enter yourself
   (so you can still edit before sending).
5. **Press Esc (or Ctrl+C) to quit** — that frees the GPU VRAM.

Keep the target window focused while speaking and ~1–2 s after release.

## Flags

| Flag | Effect |
|---|---|
| `--key f9` | hold-to-talk key (`ctrl_r`, `alt_r`, `f9`…) |
| `--paste` | inject via clipboard + Ctrl+V (instant, atomic — **no dropped spaces**; clobbers clipboard) |
| `--no-type` | just print what was heard (testing) |
| `--model NAME` | Whisper model (default `large-v3-turbo`, the fastest; env `VOICEPI_MODEL`) |
| `--device D` | `auto`/`cuda`/`cpu` (default `auto`; env `VOICEPI_DEVICE`) |
| `--lang CODE` | spoken-language hint `da`/`en`/`de`/`fr`… (default `da`; env `VOICEPI_LANG`) — reliable on short/soft speech |
| `--autodetect` | let Whisper guess the language (less reliable on short/soft speech) |

Default injection = keystroke typing: universal, works in any text
input incl. non-ASCII, no paste-keybinding assumptions. **Use
`--paste`** if words run together or typing is too slow — keystroke
typing can outrun the focused app and drop spaces; clipboard paste is
atomic and instant.

## Tuning

| Env | Default | Effect |
|---|---|---|
| `VOICEPI_TARGET_DBFS` | `-20` | lower (e.g. `-16`) = boost quiet speech harder |
| `VOICEPI_MODEL` | `large-v3-turbo` | the fastest; `large-v3` = slightly better soft-speech accuracy, slower |
| `VOICEPI_DEVICE` | `auto` | `cuda` / `cpu` to force; `auto` = NVIDIA if present, else CPU |
| `VOICEPI_LANG` | `da` | spoken-language hint (`en`, `de`, `fr`, …) |

VAD threshold / temperature ladder are in `voice_pi.py` (`_transcribe`).
The `[cap]` / `[stt]` lines show loudness, gain, noise floor, SNR and
per-utterance `compute=` time — read `snr` to judge mic quality:
≳25 dB excellent, 15–25 dB workable, <15 dB the mic/room is the limit.

## Notes

- The real soft-speech accuracy test is your own voice + mic.
- Possible later: hands-free VAD mode instead of push-to-talk. PTT is
  the robust default for quiet speech — no false triggers.

## License

MIT — see [LICENSE](LICENSE).
