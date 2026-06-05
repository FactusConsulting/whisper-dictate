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
installed files, a clean upgrade wipes them.

`config.json` is read before env-var fallback for every matching setting, so
the settings UI can own your normal defaults while old `setx VOICEPI_*` values
still work for keys that are absent from the JSON file.

`config.json` can also contain target profiles. Profiles match the active
window title/process captured when recording starts, then their `settings`
override the normal config for that utterance. Live-safe settings apply
immediately; restart-only settings such as backend/model/device are reported as
requiring restart/model reload.

## Cheat sheet — every knob at a glance

| Knob | Env var | CLI flag | Default | Range / options | What it does |
|---|---|---|---|---|---|
| **Whisper model** | `VOICEPI_MODEL` | `--model` | `large-v3-turbo` | `large-v3-turbo`, `large-v3`, `medium`, `small`, `base`, `tiny`, `distil-large-v3`, … | turbo = fastest default; `large-v3` = best accuracy |
| **STT backend** | `VOICEPI_STT_BACKEND` | _none_ | `whisper` | `whisper` \| `parakeet` \| `openai` | default uses faster-whisper; `openai` sends audio to an external OpenAI-compatible API |
| **External STT model** | `VOICEPI_STT_MODEL` | _none_ | _(unset)_ | `gpt-4o-mini-transcribe`, `gpt-4o-transcribe`, `whisper-1`, compatible names | model used when `VOICEPI_STT_BACKEND=openai` |
| **External STT URL** | `VOICEPI_STT_BASE_URL` | _none_ | `https://api.openai.com/v1` | URL | OpenAI-compatible transcription API base URL |
| **Device** | `VOICEPI_DEVICE` | `--device` | `auto` | `auto` \| `cuda` \| `cpu` | auto picks NVIDIA GPU if present, else CPU |
| **Compute type / precision** | `VOICEPI_COMPUTE_TYPE` | _none_ | `int8_float16` (GPU) / `int8` (CPU) | `int8`, `int8_float16`, `float16`, `bfloat16`, `float32` | Whisper/CTranslate2 precision override — Parakeet currently ignores this setting; see VRAM table below |
| **Spoken language** | `VOICEPI_LANG` | `--lang` / `--autodetect` | _(unset → auto-detect)_ | ISO 639-1: `da`, `en`, `de`, `fr`, `sv`, `nb`, `nl`, `fi`, `pl`, `pt`, `es`, `it`, `uk`, … | Whisper language hint; Parakeet v3 autodetects language and does not use this setting |
| **Beam-search width** | `VOICEPI_BEAM_SIZE` | _none_ | `1` | integer ≥ 1 (typical 1-16) | wider = more accurate, slower (cheap on GPU) |
| **Decode temperatures** | `VOICEPI_TEMPERATURE` | _none_ | `0.0,0.2` | CSV floats (e.g. `0.0`, `0.0,0.2,0.4`) | Whisper's fallback ladder. `0.0` locks to greedy decode = predictable output, no "creative" fallback on uncertainty. |
| **Context for long ytringer** | `VOICEPI_CONTEXT_MIN_SECONDS` | _none_ | `0` (off) | float seconds (`0` = disabled, `5` = enable for utterances ≥ 5 s) | Pass `condition_on_previous_text=True` only when an utterance is at least this long. Helps Whisper keep word boundaries on long sentences without triggering hallucinations on short ones. |
| **Parakeet minimum utterance** | `VOICEPI_PARAKEET_MIN_SECONDS` | _none_ | `1.5` | float seconds (`0` disables) | Ignore very short Parakeet recordings because multilingual language autodetection is weaker on short clips. |
| **Release tail padding** | `VOICEPI_RELEASE_TAIL_MS` | _none_ | `200` | integer ms (`0` disables) | continue capturing briefly after hotkey release so final syllables/words are not clipped. |
| **Vocabulary hint** | `VOICEPI_INITIAL_PROMPT` | _none_ | _(unset)_ | free text up to ~1024 chars | bias toward your domain words/names |
| **Custom dictionary** | `VOICEPI_DICTIONARY` | _none_ | user config path | JSON/text file path(s) | bounded vocabulary prompt + exact smart replacements for names like `Claude Code`, `Codex`, `OpenClaw` |
| **Push-to-talk key** | `VOICEPI_KEY` | `--key` | `ctrl_r` | pynput key name (`ctrl_r`, `alt_r`, `f9`, …) or `a+b` chord | hold-to-talk key |
| **Inject mode** | `VOICEPI_INJECT_MODE` | `--type` / `--paste` / `--no-type` | `auto` | `auto` \| `type` \| `paste` \| `print` | auto-select injection strategy, force typing, force clipboard paste (X11/Win), or print-only |
| **Format commands** | `VOICEPI_FORMAT_COMMANDS` | _none_ | `off` | `off` \| `en` \| `da` \| `both` | Optional deterministic spoken formatting commands. English supports `new line`, `comma`, `period`; Danish supports `ny linje`, `komma`, `punktum`. |
| **Global quit key** | `VOICEPI_QUIT_KEY` | _none_ | `esc` | pynput key name or one character | key used for the global quit shortcut (Windows/X11) |
| **Global quit count** | `VOICEPI_QUIT_COUNT` | _none_ | `3` | integer ≥ 0 (`0` disables) | N consecutive quit-key presses to quit (Windows/X11) |
| **Quit window** | `VOICEPI_QUIT_WINDOW_MS` | _none_ | `1500` | integer ms | time window for the consecutive quit-key presses |
| **Audio loudness target** | `VOICEPI_TARGET_DBFS` | _none_ | `-20` | float dBFS ≤ 0 | target for quiet-boost normalisation |
| **Audio min input** | `VOICEPI_MIN_INPUT_DBFS` | _none_ | `-55` | float dBFS | reject input quieter than this |
| **Audio min SNR** | `VOICEPI_MIN_SNR_DB` | _none_ | `6` | float dB | reject input below this speech-vs-noise contrast |
| **XKB layout (Wayland)** | `VOICEPI_XKB_LAYOUT` (highest), `XKB_DEFAULT_LAYOUT` (fallback) | _none_ | _(auto-detect)_ | `dk`, `se`, `de`, `fi`, `no`, `es`, `pt`, `br`, `pl`, `ua`, … | force keycode layout for special-char injection |
| **JSON output** | `VOICEPI_JSON` | `--json` | _(unset)_ | truthy / falsey | print one structured JSON event per accepted utterance |
| **Metrics file** | `VOICEPI_METRICS_JSONL` | _none_ | _(unset)_ | file path | append one structured JSON event per accepted utterance |
| **Command hook** | `VOICEPI_COMMAND_HOOK` | _none_ | _(unset)_ | command string or JSON string array | Advanced opt-in automation hook. Receives one utterance JSON event on stdin and is executed without shell interpolation. |
| **Command hook timeout** | `VOICEPI_COMMAND_HOOK_TIMEOUT_MS` | _none_ | `2000` | integer ms | Maximum wait for the command hook. Timeout/failure is logged and recorded but does not block injection success. |
| **Local history** | `VOICEPI_HISTORY_ENABLED` | _none_ | `1` | truthy / falsey | store accepted live dictations locally for copy/reinject/debug recovery |
| **History file** | `VOICEPI_HISTORY_JSONL` | _none_ | user state path | file path | override the local history JSONL path |
| **Local only** | `VOICEPI_LOCAL_ONLY` | _none_ | _(unset)_ | truthy / falsey | block cloud/BYOK backends and force model libraries into offline mode |
| **Post processor** | `VOICEPI_POST_PROCESSOR` | _none_ | `none` | `none` \| `ollama` \| `openai` | optional second text pass after STT and dictionary replacements |
| **Post mode** | `VOICEPI_POST_MODE` | _none_ | `raw` | `raw`, `clean`, `prompt`, `terminal`, `slack`, `email`, `bullets` (`bullet-list` alias) | rewrite style for the optional second pass |
| **Post model** | `VOICEPI_POST_MODEL` | _none_ | `qwen2.5:3b` | Ollama model name | local text model used by the post processor |
| **Post base URL** | `VOICEPI_POST_BASE_URL` | _none_ | `http://localhost:11434` | URL | local Ollama endpoint |
| **Post timeout** | `VOICEPI_POST_TIMEOUT_MS` | _none_ | `2000` | integer ms | fallback to dictionary-final text if local rewrite is too slow |
| **STT segment debug** | `VOICEPI_STT_DEBUG` | _none_ | _(unset)_ | truthy / falsey | print Whisper segment metadata (`avg_logprob`, `no_speech_prob`, `compression_ratio` when available) |
| **Disable terminal color** | `VOICEPI_NO_COLOR` / `NO_COLOR` | _none_ | _(unset)_ | any non-empty | keep terminal status lines plain even when stdout is interactive |
| **VAD threshold** | `VOICEPI_VAD_THRESHOLD` | _none_ | `0.3` | float | Silero VAD speech threshold passed to faster-whisper |
| **VAD silence** | `VOICEPI_VAD_MIN_SILENCE_MS` | _none_ | `600` | integer ms | minimum silence gap used by VAD segmentation |
| **Skip syscheck** | `VOICEPI_SKIP_SYSCHECK` | _none_ | _(unset)_ | any non-empty | skip `ubuntu26.04/setup.sh` apt-dep check (auto-set by brew/nix) |
| **Debug dump** | `VOICEPI_DEBUG` | _none_ | _(unset)_ | `1` / `true` / any truthy | log every effective setting at startup |

The detailed tables below are the same knobs split by surface (env vars
vs flags) with the longer prose. Most users only need the cheat sheet +
the **GPU VRAM sizing** table further down.

## Environment variables

| Variable | Default | Values | Effect |
|---|---|---|---|
| `VOICEPI_MODEL` | `large-v3-turbo` | any faster-whisper model: `large-v3-turbo`, `large-v3`, `medium`, `small`, `base`, `tiny`, `distil-large-v3` … | Whisper model. `large-v3-turbo` = fastest (default); `large-v3` = best accuracy, slower. Also `--model`. |
| `VOICEPI_STT_BACKEND` | `whisper` | `whisper` \| `parakeet` \| `openai` | Selects the STT engine. `whisper` is recommended for Danish accuracy. `parakeet` loads NVIDIA NeMo lazily, is experimental on Windows, is very fast on NVIDIA CUDA, and uses `nvidia/parakeet-tdt-0.6b-v3` when the normal Whisper default model is unchanged. `openai` sends recorded audio to an OpenAI-compatible transcription API and is blocked by `VOICEPI_LOCAL_ONLY=1`. |
| `stt_provider` in `config.json` | `openai` | `openai` \| `groq` | Rust UI provider selector for cloud STT. It sets `VOICEPI_STT_BASE_URL` and provider-specific model choices for the managed worker. Existing configs with a Groq URL are migrated to `groq`. |
| `VOICEPI_STT_MODEL` | *(unset → `gpt-4o-mini-transcribe` for local Whisper names)* | `gpt-4o-mini-transcribe`, `gpt-4o-transcribe`, `whisper-1`, or compatible model name | External transcription model used only when `VOICEPI_STT_BACKEND=openai`. If you leave `VOICEPI_MODEL=large-v3-turbo`/`large-v3`, the adapter maps it to `gpt-4o-mini-transcribe` unless this is set. |
| `VOICEPI_STT_BASE_URL` | `https://api.openai.com/v1` | URL | OpenAI-compatible transcription API base URL used only when `VOICEPI_STT_BACKEND=openai`. |
| `VOICEPI_STT_TIMEOUT_MS` | `30000` | integer ms | Maximum wait for an external transcription request. |
| `VOICEPI_STT_API_KEY` / `GROQ_API_KEY` / `OPENAI_API_KEY` | *(unset)* | API key | Bearer token for `VOICEPI_STT_BACKEND=openai`. `VOICEPI_STT_API_KEY` takes precedence; `GROQ_API_KEY` is used when `VOICEPI_STT_BASE_URL` points at Groq; `OPENAI_API_KEY` is the generic fallback. The Rust UI can store provider-specific STT keys in the OS credential store and passes them to the managed worker as `VOICEPI_STT_API_KEY`; env vars remain supported for terminal runs. |
| `VOICEPI_PARAKEET_MODEL` | `nvidia/parakeet-tdt-0.6b-v3` | NeMo ASR model name | Optional Parakeet-specific model override. Takes precedence over `VOICEPI_MODEL` when `VOICEPI_STT_BACKEND=parakeet`. |
| `VOICEPI_DEVICE` | `auto` | `auto` \| `cuda` \| `cpu` | Compute device. `auto` = NVIDIA GPU if present, else CPU. Invalid value → error. Also `--device`. |
| `VOICEPI_COMPUTE_TYPE` | *(unset → `int8_float16` on GPU, `int8` on CPU)* | `int8` \| `int8_float16` \| `float16` \| `bfloat16` \| `float32` … (any ctranslate2-supported type) | Whisper-only precision override for faster-whisper/CTranslate2. Big-GPU users gain accuracy with `float16` (or `bfloat16` on Ampere/Ada+); `int8_float16` defaults trade a little accuracy for VRAM/speed. Parakeet uses PyTorch/NeMo and currently ignores this setting, so the UI disables it when Parakeet is selected. |
| `VOICEPI_LANG` | *(unset → auto-detect)* | ISO 639-1: `da en de fr sv nb nn nl fi pl pt es it uk` … (any Whisper language); empty/unset = auto-detect | Whisper language hint. Strongly recommended for short/soft Whisper dictation. Parakeet v3 autodetects language and does not use this setting. Also `--lang`. |
| `VOICEPI_KEY` | `ctrl_r` | pynput key name, or chord `a+b` | Hold-to-talk key. e.g. `ctrl_r`, `alt_r`, `shift_r`, `f9`, or `shift_r+ctrl_r` (hold both). Also `--key`. |
| `VOICEPI_BEAM_SIZE` | `1` | integer ≥ 1 (typical `1`–`5`) | Beam-search width. `1` = fastest; `5` = better accuracy, 3–4× slower on CPU (cheap on GPU). Env only — no flag. |
| `VOICEPI_INITIAL_PROMPT` | *(none)* | free text | Context/vocabulary hint biasing recognition toward your terms/names. Env only. |
| `VOICEPI_DICTIONARY` | platform user config path | path list (`;` on Windows, `:` on Unix) | Load one or more custom dictionaries. JSON supports `terms` and `replacements`; text files support `[terms]` / `[replacements]`. Terms are appended to the Whisper prompt within the configured limits; replacements run after transcription. Env only. |
| `VOICEPI_DICTIONARY_ENABLED` | `1` | truthy / falsey | Set `0`, `false`, `no`, or `off` to disable dictionary loading without removing the file. |
| `VOICEPI_DICTIONARY_MAX_TERMS` | `80` | integer ≥ 0 | Maximum number of dictionary terms appended to the prompt. Keeps prompt injection bounded as the dictionary grows. |
| `VOICEPI_DICTIONARY_PROMPT_CHARS` | `1200` | integer ≥ 0 | Maximum total characters used by dictionary terms in the prompt. |
| `VOICEPI_INJECT_MODE` | `auto` | `auto` \| `type` \| `paste` \| `print` | Controls text output injection. `auto` types directly except for known fragile Windows terminal targets, where it uses clipboard paste. `type` always sends direct keystrokes, `paste` copies the text to the clipboard and sends paste on X11/Windows, and `print` only writes the transcription to stdout. `--type`/`--paste`/`--no-type` override this env var. |
| `VOICEPI_FORMAT_COMMANDS` | `off` | `off` \| `en` \| `da` \| `both` | Opt-in deterministic spoken formatting pass after STT/dictionary/post-processing and before injection. Examples: `new line`, `comma`, `period`, `ny linje`, `komma`, `punktum`, `question mark`, `spørgsmålstegn`, `bullet list`, `punktliste`. Profile-safe and live-reloadable. |
| `VOICEPI_COMMAND_HOOK` | *(unset)* | command string or JSON string array | Optional advanced automation command run after each accepted live utterance. The full utterance event is sent as JSON on stdin. The command is started directly with `shell=False`; transcript text is never shell-interpolated. Prefer JSON array form such as `["python","D:\\scripts\\hook.py"]`. |
| `VOICEPI_COMMAND_HOOK_TIMEOUT_MS` | `2000` | integer ms | Maximum wait for `VOICEPI_COMMAND_HOOK`. Timeout/failure is logged and included in metrics/history as `command_hook_*` fields, but text injection is not undone or blocked. |
| `VOICEPI_QUIT_KEY` | `esc` | pynput key name or one character | **Windows/X11 only** (pynput path). Key used for global quit. Examples: `esc`, `f12`, `q`. |
| `VOICEPI_QUIT_COUNT` | `3` | integer ≥ 0 | **Windows/X11 only** (pynput path). N consecutive `VOICEPI_QUIT_KEY` presses within `VOICEPI_QUIT_WINDOW_MS` quit the app. Default `3` avoids accidental shutdown since pynput catches keys system-wide. Set `0` to disable global key quit entirely (rely on Ctrl+C in the launcher console); set `1` for single-key behaviour. |
| `VOICEPI_QUIT_WINDOW_MS` | `1500` | integer ms | Time window within which the consecutive quit-key presses count toward `VOICEPI_QUIT_COUNT`. Any non-quit-key press resets the counter. |
| `VOICEPI_TARGET_DBFS` | `-20` | float (dBFS, ≤ 0) | Loudness quiet input is normalised toward. Lower (e.g. `-16`) = boost quiet speech harder. |
| `VOICEPI_MIN_INPUT_DBFS` | `-55` | float (dBFS) | Reject utterances quieter than this ("input too quiet"). |
| `VOICEPI_MIN_SNR_DB` | `6` | float (dB) | Reject utterances with SNR below this ("no speech contrast"). |
| `VOICEPI_XKB_LAYOUT` | *(unset)* | XKB layout name: `dk se de fi no es pt br pl ua` … | **Wayland only.** Force the keycode layout for special-char injection, overriding auto-detection (highest priority). |
| `XKB_DEFAULT_LAYOUT` | *(unset)* | XKB layout name | **Wayland only.** Also consulted (2nd priority, after `VOICEPI_XKB_LAYOUT`). `--lang` auto-sets it if unset. |
| `VOICEPI_JSON` | *(unset)* | `1` / `true` / any truthy | Print one structured JSON event per accepted utterance to stdout. Also `--json`. |
| `VOICEPI_METRICS_JSONL` | *(unset)* | file path | Append one JSON object per accepted utterance. Includes recording duration, STT compute time, real-time factor, model/device, injection strategy, target title/process, language confidence, text preview, and segment metadata. |
| `VOICEPI_HISTORY_ENABLED` | `1` | truthy / falsey | Store accepted live dictations in local history. Set `0`, `false`, `no`, or `off` to disable. |
| `VOICEPI_HISTORY_JSONL` | user state path | file path | Override the local history JSONL location. Default is `%APPDATA%\WhisperDictate\history.jsonl` on Windows and `${XDG_STATE_HOME:-~/.local/state}/whisper-dictate/history.jsonl` elsewhere. |
| `VOICEPI_LOCAL_ONLY` | *(unset)* | truthy / falsey | Privacy lock. Blocks cloud/BYOK backends and sets `HF_HUB_OFFLINE=1`, `TRANSFORMERS_OFFLINE=1`, `HF_DATASETS_OFFLINE=1`, `HF_HUB_DISABLE_TELEMETRY=1`, and Weights & Biases offline/disabled defaults before models load. Local models must already be downloaded. This is a library/runtime guard, not an OS firewall rule. |
| `VOICEPI_POST_PROCESSOR` | `none` | `none` \| `ollama` \| `openai` \| `groq` | Optional second text pass after STT and dictionary replacements. `none` disables it. `ollama` is local. `openai`/`groq` send the dictionary-final text to an OpenAI-compatible chat API and are blocked by `VOICEPI_LOCAL_ONLY=1`. |
| `VOICEPI_POST_MODE` | `raw` | `raw`, `clean`, `prompt`, `terminal`, `slack`, `email`, `bullets` (`bullet-list` alias) | Rewrite style. `raw` leaves text unchanged; `clean` fixes punctuation/casing; `prompt` rewrites for AI coding agents; `terminal` preserves commands, flags, paths and technical terms; `slack`/`email`/`bullets` format for those destinations. |
| `VOICEPI_POST_MODEL` | `qwen2.5:3b` | Ollama model name or OpenAI-compatible chat model | Text model used by the selected post processor. On 10 GB GPUs running Parakeet locally, 3B is the practical Ollama starting point. |
| `VOICEPI_POST_BASE_URL` | `http://localhost:11434` for Ollama, `https://api.openai.com/v1` for OpenAI | URL | Post-processing endpoint. With `VOICEPI_LOCAL_ONLY=1`, external providers are blocked. |
| `VOICEPI_POST_API_KEY` / `GROQ_API_KEY` / `OPENAI_API_KEY` | *(unset)* | API key | Bearer token for cloud post-processing. `VOICEPI_POST_API_KEY` takes precedence for post-processing. The Rust UI Output tab can store a separate post-processing key in the OS credential store; if none is saved, the worker can fall back to the loaded Cloud STT key. |
| `VOICEPI_POST_TIMEOUT_MS` | `2000` | integer ms | Maximum wait for the rewrite before falling back to the dictionary-final text. |
| `VOICEPI_POST_MAX_INPUT_CHARS` | `4000` | integer chars | Maximum text sent to the local post-processor. |
| `VOICEPI_POST_MAX_OUTPUT_CHARS` | `4000` | integer chars | Maximum rewritten text accepted from the local post-processor. |
| `VOICEPI_POST_REDACT` | *(unset)* | truthy / falsey | Opt-in local redaction before `VOICEPI_POST_PROCESSOR=openai`. Emails, phone numbers and common API tokens are replaced with placeholders before the cloud request and restored afterward when possible. |
| `VOICEPI_POST_REDACT_TERMS` | *(unset)* | comma-separated terms | Extra local names/terms to redact before cloud post-processing. Original values are not written to metrics; only placeholder/type/count metadata is recorded. |
| `VOICEPI_STT_DEBUG` | *(unset)* | `1` / `true` / any truthy | Print per-segment Whisper metadata when available. Useful for diagnosing hallucinations and low-confidence output. |
| `VOICEPI_NO_COLOR` / `NO_COLOR` | *(unset)* | any non-empty value | Disable ANSI styling for interactive terminal status lines. Piped output, logs, JSON, and the Rust UI stay plain automatically. |
| `VOICEPI_VAD_THRESHOLD` | `0.3` | float | Silero VAD speech threshold passed to faster-whisper. Higher rejects more non-speech but can clip quiet speech. |
| `VOICEPI_VAD_MIN_SILENCE_MS` | `600` | integer ms | Minimum silence gap used by VAD segmentation. Lower can reduce latency on clipped phrases; higher keeps phrases together. |
| `VOICEPI_PARAKEET_MIN_SECONDS` | `1.5` | float seconds (`0` disables) | Parakeet-only minimum recording length. Shorter clips are ignored to avoid poor language autodetection and low-context mistakes. |
| `VOICEPI_RELEASE_TAIL_MS` | `200` | integer milliseconds (`0` disables) | Extra audio captured after the hotkey is released. Useful when the last syllable or word is clipped because the key is released slightly before speech fully ends. Live-reloadable. |
| `VOICEPI_AUDIO_DUCKING` | *(unset)* | truthy / falsey | Windows-only optional audio ducking. While recording, other app audio sessions are lowered and restored before transcription starts. Disabled by default. |
| `VOICEPI_AUDIO_DUCKING_LEVEL` | `0.25` | float 0.0-1.0 | Target volume for other app sessions while recording when audio ducking is enabled. |
| `VOICEPI_SKIP_SYSCHECK` | *(unset)* | any non-empty value | Linux: skip the `ubuntu26.04/setup.sh` apt dependency check. Set automatically by the Homebrew/Nix wrappers; rarely set by hand. |
| `VOICEPI_DEBUG` | *(unset)* | `1` / `true` / any truthy (empty, `0`, `false`, `no`, `off` = disabled) | At startup, prints a `[debug] effective settings:` block listing every setting + which env var supplied it. Useful for "is my `setx` actually arriving in the running process?" — run with `VOICEPI_DEBUG=1` and the first lines of the log show the truth. Zero runtime cost when unset. |

See [MICROPHONE.md](MICROPHONE.md) for what the capture-tuning dBFS/SNR
numbers mean in practice.

### Probing a hotkey before you commit — `scripts/dev/probe-key.py`

Before `setx VOICEPI_KEY <something>`, verify your OS actually delivers
that key to pynput. The repo ships a 100-line standalone probe:

```powershell
# Clone or cd into the repo, then:
python scripts/dev/probe-key.py pause          # active: confirm Pause arrives
python scripts/dev/probe-key.py ctrl_r+space   # active: confirm a chord
python scripts/dev/probe-key.py                # passive: log EVERY key event
python scripts/dev/probe-key.py f9 30          # custom 30-second window
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
  quit               3x esc within 1500ms  (env VOICEPI_QUIT_KEY=esc, VOICEPI_QUIT_COUNT=3)
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

Passed after the Rust controller (`whisper-dictate run -- ...`):

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
| `whisper-dictate model-capacity` | off | — | Show NVIDIA GPU free/total VRAM and a local model fit table from the Rust controller before loading Python or Whisper. |
| `--transcribe-file PATH` | off | audio path | Transcribe an audio file with the selected backend/config and exit. 16-bit WAV works natively; mp3/m4a/other formats require ffmpeg. Combine with `--json` for structured output. |
| `--benchmark-files PATH...` | off | audio paths | Run one or more files through benchmark backend specs and emit one JSONL event per file/backend. |
| `--benchmark-corpus PATH` | off | manifest path | Run a benchmark corpus manifest and annotate results with reference text, WER/CER and technical-term hits/misses. |
| `--benchmark-backends SPEC` | current backend | CSV specs | Backend/model specs for benchmarking, e.g. `whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3,openai:gpt-4o-mini-transcribe`. |
| `--benchmark-jsonl PATH` | stdout | file path | Append benchmark JSONL results to a file instead of stdout. |
| `--calibrate-mic [SECONDS]` | off | seconds, default `5` | Record a short mic sample, print pass/warn/fail audio diagnostics and recommended threshold settings, then exit. |
| `--calibrate-file PATH` | off | audio path | Analyze an existing audio file with the same calibration logic. Combine with `--json` for structured output. |
| `--post-process-text TEXT` | off | text | Run the configured post-processor on text and exit. Useful for testing Ollama/OpenAI text cleanup without recording audio. |
| `whisper-dictate history list [N]` | off | count, default `10` | Print recent local dictation history entries from the Rust controller and exit. |
| `whisper-dictate history last` | off | — | Print the last local dictation transcript from the Rust controller and exit. |
| `--history-copy-last` | off | — | Copy the last local dictation transcript to the clipboard and exit. |
| `--history-reinject-last` | off | — | Paste the last local dictation transcript into the active window and exit. |

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

One-off via terminal (the installer put the Rust controller on PATH):

```powershell
& "$env:LOCALAPPDATA\Programs\WhisperDictate\whisper-dictate.exe" run --key ctrl_r --lang da --model large-v3 --device cuda
```

Or make your **own** shortcut whose Target is
`%LOCALAPPDATA%\Programs\WhisperDictate\whisper-dictate.exe run --key ctrl_r --lang da`

### Optional NVIDIA Parakeet backend

The default backend remains faster-whisper. To try NVIDIA Parakeet, install the
normal requirements first, then the optional NeMo requirements. This path is
experimental on Windows because NeMo/PyTorch wheel compatibility depends on the
local CUDA/Python combination:

```powershell
& "$env:USERPROFILE\voice-pi-venv\Scripts\python.exe" -m pip install `
  -r "$env:LOCALAPPDATA\Programs\WhisperDictate\requirements\parakeet.txt"
setx VOICEPI_STT_BACKEND parakeet
```

`VOICEPI_STT_BACKEND=parakeet` loads NeMo only when transcription starts, so
`--help`, `--doctor`, and the default Whisper backend do not require Parakeet
dependencies. With the default `VOICEPI_MODEL=large-v3-turbo`, the adapter uses
`nvidia/parakeet-tdt-0.6b-v3`; set `VOICEPI_PARAKEET_MODEL` or `--model` to use
another NeMo ASR model. The Settings UI intentionally lists only the practical
choices: `nvidia/parakeet-tdt-0.6b-v3` for Danish/mixed Danish-English
dictation, `nvidia/parakeet-tdt-1.1b` for pure English quality experiments, and
`nvidia/parakeet-tdt-0.6b-v2` as a fast English-only baseline. v3 autodetects
language; `VOICEPI_LANG=da` is a Whisper hint and does not force Parakeet to
Danish. NVIDIA's model card does not publish a minimum utterance length for
reliable language detection; keep `VOICEPI_PARAKEET_MIN_SECONDS` above zero for
short push-to-talk clips and tune empirically. NeMo may emit training/dataloader
/ffmpeg startup logs during model load and progress logs during transcription;
whisper-dictate hides those by default and shows them only when
`VOICEPI_STT_DEBUG=1`.

### Optional external API backends

External providers are explicit opt-in and are not used by default. For
OpenAI-compatible transcription, set an API key and switch the STT backend:

```powershell
setx OPENAI_API_KEY "sk-..."
setx VOICEPI_STT_BACKEND openai
setx VOICEPI_STT_MODEL gpt-4o-mini-transcribe
```

For Groq cloud transcription, use the Rust UI Core tab: set
`STT backend = openai`, set `Cloud STT provider = groq`, enter the API key, and
click **Save API key**. The key is stored in the OS credential store, not in
`config.json`. Equivalent terminal setup:

```powershell
setx GROQ_API_KEY "gsk_..."
setx VOICEPI_STT_BACKEND openai
setx VOICEPI_STT_BASE_URL https://api.groq.com/openai/v1
setx VOICEPI_STT_MODEL whisper-large-v3-turbo
```

For external text cleanup after local STT/dictionary replacements, set:

```powershell
setx OPENAI_API_KEY "sk-..."
setx VOICEPI_POST_PROCESSOR openai
setx VOICEPI_POST_MODEL gpt-4o-mini
setx VOICEPI_POST_MODE clean
```

Use `VOICEPI_STT_BASE_URL` and `VOICEPI_POST_BASE_URL` for compatible endpoints
that expose `/audio/transcriptions` and `/chat/completions`. `VOICEPI_LOCAL_ONLY=1`
blocks the external STT backend and external post-processor before any request is
made.

### Desktop settings UI

On Windows, the installer adds a Start-menu **whisper-dictate** shortcut which
opens a unified control UI: it starts/stops dictation, shows the runtime log,
saves settings and restarts dictation automatically when a restart-only setting
changes. On Ubuntu with Homebrew, run the bundled setup once; it creates a
GNOME **Whisper Dictate** app launcher, login autostart entry, and opens the UI:

```bash
whisper-dictate setup-ubuntu
```

After that, open **Whisper Dictate** from the Ubuntu launcher or run:

```bash
whisper-dictate ui
```

For source or release-zip installs, install the Rust UI/controller manually:

```bash
scripts/linux/install-rust-ui.sh
whisper-dictate ui
```

The Rust UI edits `%APPDATA%\WhisperDictate\config.json`, can create/open the
dictionary file, previews dictionary prompt terms, and restarts its managed
dictation process when restart-only settings change. A running standalone
dictation process also applies live-safe changes on the next record start/stop:
language, inject mode, dictionary, VAD, audio thresholds, prompt, JSON/metrics
and debug flags. Backend, model, device, compute type and hotkey are
restart-only.

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
whisper-dictate dictionary status
whisper-dictate dictionary open
whisper-dictate dictionary add "Claude Code"
whisper-dictate dictionary replace "Cloud Code=Claude Code"
whisper-dictate run --dictionary-suggest benchmark\results.jsonl
```

On Windows, the Settings UI exposes the same suggestion flow on the Dictionary
tab. Pick a benchmark/history JSONL file, review the proposed replacements, and
apply them to the configured dictionary when they look correct.

### Target profiles

Profiles live in `%APPDATA%\WhisperDictate\config.json` on Windows or
`${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/config.json` elsewhere:

```json
{
  "profiles": [
    {
      "name": "AI terminal",
      "match": {
        "title": ["Claude Code", "Codex"],
        "process": "WindowsTerminal"
      },
      "settings": {
        "inject_mode": "paste",
        "lang": "en",
        "initial_prompt": "Codex, Claude Code, Kubernetes cluster, merge, deploy"
      }
    },
    {
      "name": "Slack",
      "match": {"title": "Slack"},
      "settings": {"inject_mode": "auto"}
    }
  ]
}
```

`title` and `process` are case-insensitive substring matches; either can be a
string or a list of strings. The first matching profile wins. Active profile is
printed as `[profile] active: ...` and included in metrics/history events.

### Injection smoke test

To test a target app without loading Whisper, focus the input field and run:

```powershell
python scripts/dev/inject-smoke.py --mode auto
python scripts/dev/inject-smoke.py --mode type
python scripts/dev/inject-smoke.py --mode paste
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
whisper-dictate run --key shift_r+ctrl_r --lang da
```

Or inline for one run:

```bash
VOICEPI_LANG=da VOICEPI_BEAM_SIZE=5 whisper-dictate run --key shift_r+ctrl_r
```

### Linux — manual Rust controller

Install the Rust controller, then use env vars or flags:

```bash
scripts/linux/install-rust-ui.sh
VOICEPI_LANG=da whisper-dictate run --key ctrl_r --lang da
```

### NixOS / Nix

`nix run` — env before the command, flags after `--`:

```bash
VOICEPI_LANG=da VOICEPI_BEAM_SIZE=5 \
  nix run github:FactusConsulting/whisper-dictate -- run --key shift_r+ctrl_r --lang da
```

NixOS module — set env in the service/user environment (e.g.
`environment.sessionVariables.VOICEPI_LANG = "da";`) and the wrapper inherits
it. `VOICEPI_XKB_LAYOUT` is auto-derived from `--lang`/the session layout; the
module already wires up ydotool/uinput for Wayland.

## GPU VRAM sizing — what to set per card

Run `whisper-dictate model-capacity` to inspect local NVIDIA GPU free/total
VRAM and get a model-fit table for Whisper, Parakeet and local Ollama
post-processing models. On Windows, the Settings UI exposes the same check on
the Core tab as **Model fit**.

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
