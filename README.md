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

## Requirements

- Windows with an **NVIDIA GPU** + a reasonably recent driver.
- It will fetch official **CPython 3.12** for you if missing (via
  `winget`). 3.13/3.14 and MinGW/MSYS Python are rejected on purpose —
  the binary wheel stack (`ctranslate2`, `onnxruntime`, `nvidia-*-cu12`)
  ships MSVC wheels for 3.12.
- ~2 GB free VRAM while running; ~1.5–3 GB disk for the model (fetched
  once into the Hugging Face cache).

## Setup — one script, portable

Copy the whole folder to any Windows machine with an NVIDIA GPU and run:

```powershell
powershell -ExecutionPolicy Bypass -File setup.ps1
```

Idempotent and self-contained: first run finds/installs Python 3.12,
builds a machine-local venv, installs deps, downloads the model and
launches; later runs validate the venv and just launch. Nothing is
hardcoded to a user or path. A different GPU needs no change —
`device="cuda"` and the `nvidia-*-cu12` runtime wheels are
card-agnostic.

Any arguments are passed straight to `voice_pi.py`; with none it
defaults to `--paste --model large-v3-turbo`:

```powershell
powershell -ExecutionPolicy Bypass -File setup.ps1                # da, turbo, paste
powershell -ExecutionPolicy Bypass -File setup.ps1 --lang de       # German
powershell -ExecutionPolicy Bypass -File setup.ps1 --autodetect    # guess language
```

Manual setup (if you'd rather not use the script):

```powershell
py -m venv %USERPROFILE%\voice-pi-venv
%USERPROFILE%\voice-pi-venv\Scripts\activate
pip install -r requirements-windows.txt
python voice_pi.py --paste --model large-v3-turbo
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
| `--model NAME` | Whisper model (default `large-v3`; env `VOICEPI_MODEL`). `large-v3-turbo` ≈ 3–5× faster |
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
| `VOICEPI_MODEL` | `large-v3` | `large-v3-turbo` = faster, slightly lower soft-speech accuracy |
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
