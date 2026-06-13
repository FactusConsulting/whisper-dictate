# Configuration reference

Every setting whisper-dictate reads, its possible values and defaults, and
how to set it on each platform. Three surfaces:

- **Desktop Settings UI** — the easiest way; the Rust settings app writes
  `%APPDATA%\WhisperDictate\config.json`. The **UI tab** column in the cheat
  sheet below names the tab each knob lives on (`—` = not in the UI, available
  only as an env var / `config.json` key).
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

## Settings reference — every knob at a glance

The full list of runtime settings is generated from the schema below. The
quickest path is the **Desktop Settings UI** (it writes `config.json` for you);
headless users set the matching `VOICEPI_*` env var or `config.json` key. For
copy-paste end-to-end setups jump to the [scenario recipes](#scenario-recipes)
further down.

<!-- BEGIN GENERATED SETTINGS REFERENCE -->
_Generated from `src/python/whisper_dictate/settings_schema.json` by `scripts/dev/gen_settings_docs.py` -- do not edit this block by hand; regenerate with `py -3.12 scripts/dev/gen_settings_docs.py`._

Every runtime setting, grouped by area. **Live** settings apply on the next record start/stop; **Restart** settings (backend, model, device, compute type, hotkey) need the worker restarted. The env var is read at startup; the same name without the `VOICEPI_` prefix, lower-cased, is the `config.json` key.

#### Core (the first-time-setup basics) -- 8 basic

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `key` | `VOICEPI_KEY` | `ctrl_r` | Restart | Hold-to-talk hotkey, e.g. ctrl_r, alt_r, f9, or a chord like shift_r+ctrl_r. An all-bare-modifier binding fires only on that exact combo. |
| `model` | `VOICEPI_MODEL` | `large-v3-turbo` | Restart | Local Whisper model. large-v3-turbo = fastest default; large-v3 = best accuracy, slower. |
| `stt_backend` | `VOICEPI_STT_BACKEND` | `whisper` | Restart | Speech-to-text engine: whisper (local faster-whisper), parakeet (local NVIDIA NeMo), or openai (external OpenAI-compatible cloud API). |
| `device` | `VOICEPI_DEVICE` | `auto` | Restart | Compute device for local STT: auto picks an NVIDIA GPU if present, else CPU; force with cuda or cpu. |
| `compute_type` | `VOICEPI_COMPUTE_TYPE` | _(unset)_ | Restart | Whisper/CTranslate2 precision override (int8, int8_float16, float16, bfloat16, float32). Defaults to int8_float16 on GPU, int8 on CPU; ignored by Parakeet. |
| `audio_device` | `VOICEPI_AUDIO_DEVICE` | _(unset)_ | Live | Microphone/capture device: empty = OS default, an integer device index, or a case-insensitive name substring (e.g. Yeti). Backend-independent. |
| `lang` | `VOICEPI_LANG` | _(unset)_ | Live | Spoken-language hint as an ISO 639-1 code (da, en, de, ...). Empty = auto-detect. Strongly recommended for Whisper; Parakeet v3 autodetects and ignores it. |
| `inject_mode` | `VOICEPI_INJECT_MODE` | `auto` | Live | Text output strategy: auto (type, paste on fragile Windows terminals), type (direct keystrokes), paste (clipboard + paste on X11/Windows), or print (stdout only). |

#### Local speech-to-text (Whisper / Parakeet)

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `parakeet_model` | `VOICEPI_PARAKEET_MODEL` | _(unset)_ | Restart | Optional Parakeet/NeMo model override (e.g. nvidia/parakeet-tdt-0.6b-v3); takes precedence over model when stt_backend=parakeet. |
| `initial_prompt` | `VOICEPI_INITIAL_PROMPT` | _(unset)_ | Live | Free-text vocabulary/context hint (up to ~1024 chars) biasing recognition toward your domain words and names. |
| `beam_size` | `VOICEPI_BEAM_SIZE` | `1` | Live | Whisper beam-search width. 1 = fastest; wider = more accurate and slower (cheap on GPU). |
| `temperature` | `VOICEPI_TEMPERATURE` | `0.0,0.2` | Live | Whisper decode-temperature fallback ladder (CSV floats). 0.0 locks to greedy decode for predictable output with no creative fallback. |
| `context_min_seconds` | `VOICEPI_CONTEXT_MIN_SECONDS` | `5` | Live | Pass condition_on_previous_text only for utterances at least this long (seconds; 0 disables), keeping word boundaries on long sentences without short-clip hallucinations. |
| `hallucination_guard` | `VOICEPI_HALLUCINATION_GUARD` | `1` | Live | Local Whisper only: enable word timestamps + hallucination_silence_threshold to skip long silent gaps where Whisper invents subtitle-style text. No-op for cloud/Parakeet. |
| `max_chars_per_second` | `VOICEPI_MAX_CHARS_PER_SECOND` | `30` | Live | Speech-rate plausibility gate: drop a transcript whose chars/second exceeds this (0 disables). Real speech is ~15-25 chars/s; impossible rates flag a hallucination. |
| `min_record_seconds` | `VOICEPI_MIN_RECORD_SECONDS` | `0.5` | Live | Discard recordings shorter than this as accidental key taps (effective floor max(0.3, value)), avoiding hallucinated credits on quiet sub-second taps. |
| `parakeet_min_seconds` | `VOICEPI_PARAKEET_MIN_SECONDS` | `1.5` | Live | Parakeet only: ignore recordings shorter than this (seconds; 0 disables) because multilingual language autodetection is weaker on short clips. |
| `preview_seconds` | `VOICEPI_PREVIEW_SECONDS` | `3` | Live | Local Whisper only: re-transcribe the buffer this often (seconds; 0 disables) so the live Runtime card shows the sentence growing. Display-only. |

#### Cloud speech-to-text (OpenAI-compatible APIs)

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `stt_model` | `VOICEPI_STT_MODEL` | _(unset)_ | Restart | External transcription model used only when stt_backend=openai, e.g. gpt-4o-mini-transcribe, gpt-4o-transcribe, whisper-1, or a compatible name. |
| `stt_base_url` | `VOICEPI_STT_BASE_URL` | `https://api.openai.com/v1` | Restart | OpenAI-compatible transcription API base URL, used only when stt_backend=openai (e.g. https://api.groq.com/openai/v1 for Groq). |
| `stt_timeout_ms` | `VOICEPI_STT_TIMEOUT_MS` | `30000` | Restart | Maximum wait (ms) for an external transcription request before it is abandoned. |
| `local_only` | `VOICEPI_LOCAL_ONLY` | _(unset)_ | Restart | Privacy lock: block cloud/BYOK backends and force model libraries into offline mode (HF/Transformers/W&B offline). A library/runtime guard, not an OS firewall rule. |

#### Audio capture & voice activity

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `release_tail_ms` | `VOICEPI_RELEASE_TAIL_MS` | `200` | Live | Keep capturing briefly (ms; 0 disables) after the hotkey is released so final syllables/words are not clipped. |
| `max_record_s` | `VOICEPI_MAX_RECORD_S` | `120` | Live | Maximum recording length (seconds; 0 disables the cap). Beyond it, further audio is dropped with a warning; audio up to the cap is still transcribed. |
| `vad_threshold` | `VOICEPI_VAD_THRESHOLD` | `0.3` | Live | Silero VAD speech threshold passed to faster-whisper. Higher rejects more non-speech but can clip quiet speech. |
| `vad_min_silence_ms` | `VOICEPI_VAD_MIN_SILENCE_MS` | `600` | Live | Minimum silence gap (ms) used by VAD segmentation. Lower can cut latency on clipped phrases; higher keeps phrases together. |
| `vad_speech_pad_ms` | `VOICEPI_VAD_SPEECH_PAD_MS` | `200` | Live | Padding (ms) kept around detected speech so soft first/last syllables are not trimmed. |
| `target_dbfs` | `VOICEPI_TARGET_DBFS` | `-20` | Live | Loudness target (dBFS, <= 0) for quiet-boost normalisation. Lower (e.g. -16) boosts quiet speech harder. |
| `min_input_dbfs` | `VOICEPI_MIN_INPUT_DBFS` | `-55` | Live | Reject utterances quieter than this (dBFS) as 'input too quiet'. |
| `min_snr_db` | `VOICEPI_MIN_SNR_DB` | `6` | Live | Reject utterances with speech-vs-noise contrast below this (dB) as 'no speech contrast'. |
| `audio_ducking` | `VOICEPI_AUDIO_DUCKING` | _(unset)_ | Live | Windows only: while recording, lower other apps' audio sessions and restore them before transcription. Disabled by default. |
| `audio_ducking_level` | `VOICEPI_AUDIO_DUCKING_LEVEL` | `0.25` | Live | Target volume (0.0-1.0) for other apps' audio while recording when audio ducking is enabled. |

#### Dictionary & post-processing

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `format_commands` | `VOICEPI_FORMAT_COMMANDS` | `off` | Live | Opt-in deterministic spoken formatting commands: off, en (new line/comma/period), da (ny linje/komma/punktum), or both. |
| `dictionary` | `VOICEPI_DICTIONARY` | _(unset)_ | Live | Path(s) to custom dictionary file(s) providing a bounded vocabulary prompt plus exact smart replacements for product/app names. |
| `dictionary_enabled` | `VOICEPI_DICTIONARY_ENABLED` | `1` | Live | Toggle dictionary loading without removing the file. Set 0/false/no/off to disable. |
| `dictionary_max_terms` | `VOICEPI_DICTIONARY_MAX_TERMS` | `80` | Live | Maximum number of dictionary terms appended to the Whisper prompt, keeping prompt injection bounded as the dictionary grows. |
| `dictionary_prompt_chars` | `VOICEPI_DICTIONARY_PROMPT_CHARS` | `1200` | Live | Maximum total characters used by dictionary terms in the prompt. |
| `post_processor` | `VOICEPI_POST_PROCESSOR` | `none` | Live | Optional second text pass after STT and dictionary replacements: none, ollama (local), or openai/groq (cloud, blocked by local_only). |
| `post_mode` | `VOICEPI_POST_MODE` | `raw` | Live | Rewrite style for the post-processor: raw, clean, prompt, terminal, slack, email, or bullets (bullet-list alias). |
| `post_model` | `VOICEPI_POST_MODEL` | `qwen2.5:3b` | Live | Text model used by the selected post-processor: an Ollama model name or an OpenAI-compatible chat model. |
| `post_base_url` | `VOICEPI_POST_BASE_URL` | `http://localhost:11434` | Live | Post-processing endpoint (local Ollama by default; an OpenAI-compatible /chat/completions URL for cloud). Blocked for external providers by local_only. |
| `post_timeout_ms` | `VOICEPI_POST_TIMEOUT_MS` | `4000` | Live | Base/floor wall-clock budget (ms) for the rewrite; the effective timeout scales with length (+20 ms/char up to a 30 s ceiling) then falls back to dictionary-final text. |
| `post_max_input_chars` | `VOICEPI_POST_MAX_INPUT_CHARS` | `4000` | Live | Maximum number of characters sent to the post-processor. |
| `post_max_output_chars` | `VOICEPI_POST_MAX_OUTPUT_CHARS` | `4000` | Live | Maximum number of rewritten characters accepted back from the post-processor. |
| `post_redact` | `VOICEPI_POST_REDACT` | _(unset)_ | Live | Opt-in local redaction before cloud post-processing: replace emails, phone numbers and common API tokens with placeholders, restored afterward when possible. |
| `post_redact_terms` | `VOICEPI_POST_REDACT_TERMS` | _(unset)_ | Live | Extra comma-separated names/terms to redact before cloud post-processing. Original values are never written to metrics. |

#### Injection, hotkeys & feedback

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `xkb_layout` | `VOICEPI_XKB_LAYOUT` | _(unset)_ | Live | Wayland only: force the keycode layout for special-character injection (dk, se, de, fi, no, ...), overriding auto-detection. |
| `feedback_sounds` | `VOICEPI_FEEDBACK_SOUNDS` | _(unset)_ | Live | Play a short audio cue on record start/stop, useful when the console is hidden (headless/autostart). Non-blocking. |
| `feedback_notify` | `VOICEPI_FEEDBACK_NOTIFY` | _(unset)_ | Live | Show a desktop notification on errors (model load/capture/injection failure). Linux via notify-send; Windows/macOS no-op for now. |
| `toggle_mode` | `VOICEPI_TOGGLE` | _(unset)_ | Restart | Toggle mode: press the hotkey to start recording, press again to stop and transcribe, instead of holding it. Restart-only. |
| `quit_key` | `VOICEPI_QUIT_KEY` | `esc` | Restart | Windows/X11 only: key used for the global quit shortcut (pynput key name or one character). |
| `quit_count` | `VOICEPI_QUIT_COUNT` | `3` | Restart | Windows/X11 only: number of consecutive quit-key presses to quit (0 disables global key quit; 1 = single-key). |
| `quit_window_ms` | `VOICEPI_QUIT_WINDOW_MS` | `1500` | Restart | Time window (ms) within which the consecutive quit-key presses count; any non-quit key resets the counter. |

#### Diagnostics, history & automation

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `json_output` | `VOICEPI_JSON` | _(unset)_ | Live | Print one structured JSON event per accepted utterance to stdout; also gates the metrics file (metrics_jsonl is only written while this is on). |
| `metrics_jsonl` | `VOICEPI_METRICS_JSONL` | _(unset)_ | Live | Append one structured JSON event per accepted utterance to this file. Only written while JSON output is enabled. |
| `command_hook` | `VOICEPI_COMMAND_HOOK` | _(unset)_ | Live | Advanced opt-in automation hook (command string or JSON array). Receives one utterance JSON event on stdin and runs without shell interpolation. |
| `command_hook_timeout_ms` | `VOICEPI_COMMAND_HOOK_TIMEOUT_MS` | `2000` | Live | Maximum wait (ms) for the command hook. Timeout/failure is logged and recorded but does not block injection. |
| `history_enabled` | `VOICEPI_HISTORY_ENABLED` | `1` | Live | Store accepted live dictations locally for copy/reinject/debug recovery. Set 0/false/no/off to disable. |
| `history_jsonl` | `VOICEPI_HISTORY_JSONL` | _(unset)_ | Live | Override the local history JSONL path (default under the per-user state dir). |
| `debug` | `VOICEPI_DEBUG` | _(unset)_ | Live | Basic diagnostics: print one concise per-utterance [health] line (mic level/SNR, model confidence, warnings). |
| `stt_debug` | `VOICEPI_STT_DEBUG` | _(unset)_ | Live | Verbose diagnostics (with debug): adds the startup effective-settings dump and per-segment STT/dictionary detail. |
| `trace` | `VOICEPI_TRACE` | _(unset)_ | Live | Trace diagnostics (with debug + stt_debug): adds full audio-device enumeration and a line per capture-open attempt. High volume; for mics that won't open. |

#### Update checks

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `update_check` | `VOICEPI_UPDATE_CHECK` | `1` | Live | UI only: periodically check the public GitHub version list and show a discreet 'update available' badge. Sends no data/telemetry; skipped when local_only is on. |
| `update_check_interval_minutes` | `VOICEPI_UPDATE_CHECK_INTERVAL_MINUTES` | `15` | Live | UI only: how often (minutes, clamped to >= 5) the in-app update check polls the public version list. |
| `update_include_prereleases` | `VOICEPI_UPDATE_INCLUDE_PRERELEASES` | `0` | Live | UI only: opt in to update notifications for release candidates (pre-releases), not just final releases. Off by default; live on the next poll. |
<!-- END GENERATED SETTINGS REFERENCE -->

### Settings not in the schema

A few `VOICEPI_*` env vars and `config.json` keys are intentionally *not* in
`settings_schema.json` (they are secrets, UI-only knobs, or rarely-tuned
advanced guards) and so are documented by hand here:

| Variable / key | Default | Values | Effect |
|---|---|---|---|
| `VOICEPI_STT_API_KEY` / `GROQ_API_KEY` / `OPENAI_API_KEY` | *(unset)* | API key | Bearer token for `stt_backend=openai`. `VOICEPI_STT_API_KEY` wins; `GROQ_API_KEY` is used when the base URL points at Groq; `OPENAI_API_KEY` is the generic fallback. The Rust UI stores provider keys in the **OS credential store** and passes them to the worker as `VOICEPI_STT_API_KEY`; headless runs use the env var. **Never** stored in `config.json`. |
| `VOICEPI_POST_API_KEY` / `GROQ_API_KEY` / `OPENAI_API_KEY` | *(unset)* | API key | Bearer token for cloud post-processing. `VOICEPI_POST_API_KEY` takes precedence; otherwise the worker can fall back to the loaded Cloud STT key. |
| `stt_provider` (`config.json`) | `openai` | `openai` \| `groq` | Rust UI cloud-STT provider selector. Sets `VOICEPI_STT_BASE_URL` and provider-specific model choices for the managed worker; existing Groq-URL configs are migrated to `groq`. |
| `ui_theme` (`config.json`) | `dark` | `dark` \| `light` | Rust settings UI visual theme. UI-only; does not restart dictation or affect the Python worker. |
| `XKB_DEFAULT_LAYOUT` | *(unset)* | XKB layout name | **Wayland only.** Consulted after `VOICEPI_XKB_LAYOUT` for special-char injection layout; `--lang` auto-sets it if unset. |
| `VOICEPI_NO_COLOR` / `NO_COLOR` | *(unset)* | any non-empty | Disable ANSI styling for interactive terminal status lines. Piped output, logs, JSON and the Rust UI stay plain automatically. |
| `VOICEPI_HALLUCINATION_SILENCE_S` | `2.0` | float seconds | Silence length above which a suspected hallucination gap is skipped. Only used when `VOICEPI_HALLUCINATION_GUARD` is on. |
| `VOICEPI_NO_SPEECH_DROP` | `0.6` | float `0`-`1` | Always-on segment scrub: drop a segment whose `no_speech_prob` is at least this AND whose `avg_logprob` <= `VOICEPI_NO_SPEECH_DROP_LOGPROB`. |
| `VOICEPI_NO_SPEECH_DROP_LOGPROB` | `-0.5` | float | Confidence ceiling for the no-speech segment scrub above. |
| `VOICEPI_SKIP_SYSCHECK` | *(unset)* | any non-empty | Linux: skip the `packaging/linux/ubuntu26.04/setup.sh` apt-dep check. Auto-set by the Homebrew/Nix wrappers. |

See [MICROPHONE.md](MICROPHONE.md) for what the capture-tuning dBFS/SNR
numbers mean in practice.

## Scenario recipes

Copy-pasteable, CLI/headless-focused setups for the four most common
configurations. Each shows a minimal `config.json` **and** the equivalent env
lines for PowerShell (Windows) and bash (Linux/macOS) — pick whichever your
launch path uses. Every key links back to its row in the
[settings reference](#settings-reference--every-knob-at-a-glance) above; only
the knobs that differ from the defaults are shown.

Where to put `config.json`:

- Windows: `%APPDATA%\WhisperDictate\config.json`
- Linux/macOS: `${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/config.json`

`config.json` is read before env-var fallback, so the keys below override your
env for matching settings. Restart-only settings (`stt_backend`, `model`,
`device`, `compute_type`, `key`) need the worker restarted; the rest apply on
the next record start/stop.

### Set up from the CLI (`--setup`) / export your config (`--export-config`)

If you do not want to hand-write `config.json`, the worker can build it for you
and dump an existing one — no desktop UI, no model download required (these
modes run before any ML dependency loads).

- **`whisper-dictate run --setup`** launches an interactive wizard driven by the
  settings schema. It walks the **basic** first-setup knobs first (showing each
  setting's description, its current value or default, and the valid choices for
  enum settings like `stt_backend`/`device`/`inject_mode`), then asks
  **`Run advanced setup? [y/N]`** before walking the rest grouped by category.
  Press ENTER to keep the shown value; type to change it; numeric settings are
  re-prompted if you exceed the schema's min/max bounds. When you pick a cloud
  backend (`stt_backend=openai`) or a cloud post-processor it prompts for the
  API key with **hidden input**. On finish it writes `config.json` to the
  standard path (printed) with only the keys that differ from the defaults, then
  prints the equivalent PowerShell (`$env:VOICEPI_X = '...'`) and bash
  (`export VOICEPI_X=...`) lines. **API keys are never written to
  `config.json`** — they are shown in the env-lines **redacted** (`***`) with a
  note to set the env var (`VOICEPI_STT_API_KEY` / `VOICEPI_POST_API_KEY`)
  yourself. The wizard is pipe-/TTY-safe: with a non-interactive stdin it reads
  scripted answers line by line instead of hanging.

- **`whisper-dictate run --export-config`** prints your **current effective
  config** — `config.json` merged with any `VOICEPI_*` env overrides, resolved
  exactly the way the worker resolves settings at startup — as a `config.json`
  blob plus copy-pasteable PowerShell and bash env-lines. Secrets are
  **redacted by default**; add **`--include-secrets`** to emit API keys in full
  (for backup/migration). Useful for snapshotting a working setup or moving it
  to another machine.

### Recipe A — Local STT on GPU (Whisper or Parakeet)

Run everything locally on an NVIDIA GPU. No network, no keys. See the
[GPU VRAM sizing](#gpu-vram-sizing--what-to-set-per-card) table to pick
`model` + `compute_type` for your free VRAM.

`config.json`:

```json
{
  "stt_backend": "whisper",
  "model": "large-v3",
  "device": "cuda",
  "compute_type": "float16",
  "beam_size": "8",
  "lang": "da"
}
```

PowerShell (persistent env, honoured by the Start-menu shortcut):

```powershell
setx VOICEPI_STT_BACKEND whisper
setx VOICEPI_MODEL large-v3
setx VOICEPI_DEVICE cuda
setx VOICEPI_COMPUTE_TYPE float16
setx VOICEPI_BEAM_SIZE 8
setx VOICEPI_LANG da
# restart whisper-dictate so the new process inherits these
```

bash:

```bash
export VOICEPI_STT_BACKEND=whisper
export VOICEPI_MODEL=large-v3
export VOICEPI_DEVICE=cuda
export VOICEPI_COMPUTE_TYPE=float16
export VOICEPI_BEAM_SIZE=8
export VOICEPI_LANG=da
```

Notes:

- **VRAM:** `large-v3` at `float16` needs roughly 4–5 GB free; on an 8 GB card
  drop to the default `int8_float16` (omit `compute_type`). Run
  `whisper-dictate model-capacity` to see free VRAM and a fit table before
  loading. If the first transcription OOMs, drop `beam_size` or step
  `compute_type` down a tier (`float16` → `int8_float16`).
- **Parakeet variant:** set `stt_backend=parakeet` and install the optional NeMo
  requirements (see [Optional NVIDIA Parakeet backend](#optional-nvidia-parakeet-backend)).
  With the default model the adapter uses `nvidia/parakeet-tdt-0.6b-v3`; override
  with `parakeet_model`. Parakeet ignores `compute_type` and autodetects language
  (so `lang` is a no-op), and keep `parakeet_min_seconds` above zero for short
  push-to-talk clips.

### Recipe B — Cloud STT + API key (Groq or OpenAI)

Send recorded audio to an OpenAI-compatible transcription API. **Headless, the
env var is the only path to the key** — the OS credential store is written and
read by the Rust UI only, so a terminal/server run must export the key itself.

`config.json` (the **key is never stored here**):

```json
{
  "stt_backend": "openai",
  "stt_provider": "groq",
  "stt_base_url": "https://api.groq.com/openai/v1",
  "stt_model": "whisper-large-v3-turbo"
}
```

PowerShell:

```powershell
setx VOICEPI_STT_BACKEND openai
setx VOICEPI_STT_BASE_URL https://api.groq.com/openai/v1
setx VOICEPI_STT_MODEL whisper-large-v3-turbo
# Key via env (headless). Prefer a per-session var over setx so it isn't
# persisted to the user registry in plain text:
$env:VOICEPI_STT_API_KEY = "gsk_..."   # or GROQ_API_KEY
```

bash:

```bash
export VOICEPI_STT_BACKEND=openai
export VOICEPI_STT_BASE_URL=https://api.groq.com/openai/v1
export VOICEPI_STT_MODEL=whisper-large-v3-turbo
export VOICEPI_STT_API_KEY="gsk_..."   # or GROQ_API_KEY
```

Notes:

- **OpenAI variant:** `stt_base_url=https://api.openai.com/v1` (the default, so
  you can omit it), `stt_model=gpt-4o-mini-transcribe`, key in `OPENAI_API_KEY`
  or `VOICEPI_STT_API_KEY`.
- **Key precedence:** `VOICEPI_STT_API_KEY` wins; `GROQ_API_KEY` is used when the
  base URL points at Groq; `OPENAI_API_KEY` is the generic fallback. The Rust UI
  saves provider keys in the OS credential store and hands the worker
  `VOICEPI_STT_API_KEY`; headless, you set it yourself.
- **Safety:** keep the key out of `config.json` and out of shell history /
  process listings where you can (use a session env var, a secrets manager, or a
  systemd `EnvironmentFile` with `0600` perms). `VOICEPI_LOCAL_ONLY=1` blocks the
  cloud backend entirely as a hard privacy lock.
- See [Optional external API backends](#optional-external-api-backends) for the
  full provider notes.

### Recipe C — Post-processing (local Ollama or remote server)

Add a second text pass after STT + dictionary replacements to clean punctuation,
reformat for Slack/email, etc. Default is local Ollama; point it at a remote
server by changing `post_base_url`.

`config.json` (local Ollama):

```json
{
  "post_processor": "ollama",
  "post_mode": "clean",
  "post_model": "qwen2.5:3b",
  "post_base_url": "http://localhost:11434",
  "post_timeout_ms": "4000"
}
```

PowerShell:

```powershell
setx VOICEPI_POST_PROCESSOR ollama
setx VOICEPI_POST_MODE clean
setx VOICEPI_POST_MODEL qwen2.5:3b
setx VOICEPI_POST_BASE_URL http://localhost:11434
```

bash (remote Ollama on another host):

```bash
export VOICEPI_POST_PROCESSOR=ollama
export VOICEPI_POST_MODE=clean
export VOICEPI_POST_MODEL=qwen2.5:3b
export VOICEPI_POST_BASE_URL=http://gpu-box.lan:11434
```

Notes:

- **Cloud post-processing:** set `post_processor=openai` (or `groq`),
  `post_base_url` to the provider's `/chat/completions` host, `post_model` to a
  chat model, and supply `VOICEPI_POST_API_KEY` (falls back to the Cloud STT
  key). Blocked by `VOICEPI_LOCAL_ONLY=1`.
- **Redaction:** before any cloud rewrite, set `post_redact=1` to replace emails,
  phone numbers and common API tokens with placeholders (restored afterward when
  possible); add your own names/terms with `post_redact_terms`. Original values
  are never written to metrics.
- **Timeout:** `post_timeout_ms` is the floor; the effective budget scales with
  text length (+20 ms/char, 30 s ceiling), then falls back to the dictionary-final
  text if the rewrite is still too slow — so a slow model degrades gracefully
  rather than dropping text.

### Recipe D — Linux / Wayland headless injection

Run as a headless autostart entry on Wayland (or X11) with no terminal visible.
The hard parts are the injection backend and the keyboard layout for special
characters.

`config.json`:

```json
{
  "inject_mode": "auto",
  "xkb_layout": "dk",
  "lang": "da",
  "feedback_sounds": "1",
  "feedback_notify": "1"
}
```

bash (e.g. in `~/.profile`, a systemd user unit, or the autostart `.desktop`
environment):

```bash
export VOICEPI_INJECT_MODE=auto
export VOICEPI_XKB_LAYOUT=dk        # force the keycode layout for æ ø å etc.
export VOICEPI_LANG=da
export VOICEPI_FEEDBACK_SOUNDS=1    # audible cue since no console is visible
export VOICEPI_FEEDBACK_NOTIFY=1    # desktop notification on errors
```

Notes:

- **Wayland injection uses `ydotool`** (direct evdev keycodes); the daemon needs
  access to `/dev/uinput`. On NixOS the module already wires up ydotool/uinput;
  on other distros install `ydotool`, run `ydotoold`, and add your user to the
  `input` group (or grant uinput access). Run `whisper-dictate run --doctor` to
  check the Wayland health prerequisites before loading Whisper.
- **`xkb_layout`** sets the keycode layout for special-character injection
  (highest priority; `XKB_DEFAULT_LAYOUT` is the fallback, and `--lang`
  auto-sets it if unset). Use `dk`, `se`, `de`, `no`, … to match your physical
  layout so `æ ø å` and friends land correctly.
- **X11 instead of Wayland:** `inject_mode` `type`/`paste` both work via pynput;
  the global quit chord (`quit_key`/`quit_count`) is available on X11/Windows but
  not Wayland.
- **What a headless server needs:** a working microphone (`whisper-dictate run
  --list-audio-devices` and `--test-audio-device "<name>"` to verify without
  loading a model), the injection backend above, and — since there is no console —
  `feedback_sounds`/`feedback_notify` for record/error cues. There is no separate
  "server mode"; it is the normal `whisper-dictate run` launched without a
  terminal (`Terminal=false` in the `.desktop` entry).

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

### Diagnostics levels — Basic `[health]` line vs. Verbose config dump

The UI **Diagnostics** dropdown maps to three env-named bools:

- **Off** (`VOICEPI_DEBUG` unset, `VOICEPI_STT_DEBUG` unset, `VOICEPI_TRACE` unset): no diagnostics.
- **Basic** (`VOICEPI_DEBUG=1`): one concise `[health]` line per utterance, e.g.

  ```
  [health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/groq
  ```

  and, when something looks off, terse warnings:

  ```
  [health] mic -55dBFS SNR 3dB too_quiet | confidence low (-0.82) | post off | WARN low confidence | WARN quiet input
  ```

  The `confidence` band is a plain read of the segments' mean `avg_logprob`
  (`high >= -0.35`, `ok -0.35..-0.60`, `low < -0.60`). Quiet-but-clean input
  is fine and does **not** warn; only quiet **and** low-SNR input does.
- **Verbose** (`VOICEPI_DEBUG=1` **and** `VOICEPI_STT_DEBUG=1`): Basic plus the
  startup `[debug] effective settings:` config dump and per-segment
  `[stt-debug]` detail.
- **Trace** (`VOICEPI_DEBUG=1` **and** `VOICEPI_STT_DEBUG=1` **and**
  `VOICEPI_TRACE=1`): the maximal level. Verbose plus the full audio-device
  enumeration at startup and a line for **every** capture-open attempt — so a
  microphone that won't open is diagnosable from the log alone, without an
  external probe script. At startup, every input device is listed:

  ```
  [trace][devices] host-apis: ['MME', 'Windows DirectSound', 'Windows WASAPI']
  [trace][devices] in dev=1 name='Microphone (Yeti Stereo Micro)' host=MME max_in_ch=2 default_sr=44100.0
  [trace][devices] in dev=51 name='Microphone (Yeti Stereo Microphone)' host=Windows WASAPI max_in_ch=2 default_sr=48000.0
  ```

  Then every capture-open attempt (host-API × samplerate × channels × dtype ×
  auto-convert) is logged with its result, plus the finally-bound candidate:

  ```
  [trace][cap] attempt host=Windows WASAPI dev=51 rate=16000 ch=2 dtype=int16 latency=low autoconv=0 -> Error opening InputStream: Unanticipated host error [PaErrorCode -9999]: 'Undefined external error.' [AUDCLNT_E_UNSUPPORTED_FORMAT]
  [trace][cap] attempt host=Windows WASAPI dev=51 rate=16000 ch=2 dtype=float32 latency=low autoconv=0 -> ok
  [trace][cap] BOUND host=Windows WASAPI dev=51 rate=16000 ch=2 dtype=float32 latency=low autoconv=0
  ```

  The **host-API coverage is the key insight**: if WASAPI rejects every format
  for a device, the log makes that obvious, so the fix (try MME/DirectSound, or
  the WASAPI auto-convert / native-rate fallbacks) is clear. Trace is high
  volume — use it only while troubleshooting, then return to **Off**/**Basic**.

### Debugging "is my `setx` arriving?" — Verbose diagnostics

A common confusion on Windows is that `setx` writes to the user registry,
but **only new processes inherit it** — a whisper-dictate launched from a
stale Start-menu shortcut or tray-restart may still see the old values.

To verify what the running process actually sees, set **Verbose**
diagnostics (`VOICEPI_DEBUG=1` and `VOICEPI_STT_DEBUG=1`) and restart. The
first lines of the log will print every effective setting + the env var that
supplied it:

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
where `$env:VOICEPI_X` shows the value. Set Diagnostics to **Off** for
normal use; **Basic** if you want the lightweight per-utterance `[health]`
line; **Verbose** adds the config dump (~10 lines on startup) and per-segment
detail; **Trace** adds the full audio-device enumeration and a line per
capture-open attempt (for diagnosing a mic that won't open). All have zero
runtime cost when their level is not selected.

## CLI flags

Passed after the Rust controller (`whisper-dictate run -- ...`):

| Flag | Default | Values | Effect |
|---|---|---|---|
| `--key` | `$VOICEPI_KEY` or `ctrl_r` | pynput key name, or chord `a+b` | Hold-to-talk key. e.g. `ctrl_r`, `alt_r`, `shift_r`, `f9`, or `shift_r+ctrl_r` (hold both). An all-bare-modifier binding (single modifier or modifier chord) activates only when that exact combo is pressed and nothing else (not inside a larger shortcut chord). |
| `--model NAME` | `$VOICEPI_MODEL` | see `VOICEPI_MODEL` | Whisper model for this run. |
| `--lang CODE` | `$VOICEPI_LANG` | ISO 639-1 code | Force language for this run. Omit to auto-detect. |
| `--autodetect` | off | — | Force language auto-detect (overrides `--lang`/`VOICEPI_LANG`). |
| `--device D` | `$VOICEPI_DEVICE` | `auto` \| `cuda` \| `cpu` | Compute device for this run. |
| `--type` | `$VOICEPI_INJECT_MODE` or off | — | Force direct keyboard typing on X11/Windows. (Wayland always uses direct evdev keycodes regardless.) |
| `--paste` | `$VOICEPI_INJECT_MODE` or off | — | Force clipboard paste: copies text via pyperclip, then sends Ctrl+V (or Ctrl+Shift+V for terminals) via ydotool on Wayland, or via pynput on X11/Windows. If the previous clipboard could be read, it is restored after a short delay — but only when the clipboard still holds the injected text (your own copy in the meantime is never overwritten). |
| `--no-type` | `$VOICEPI_INJECT_MODE` or off | — | Print the transcription only, don't inject (testing). |
| `--json` | `$VOICEPI_JSON` or off | — | Also print one structured JSON event per accepted utterance. |
| `--doctor` | off | — | Run Linux/Wayland health checks and exit before loading Whisper. |
| `--setup` | off | — | Interactive config wizard (no model load): writes `config.json` and prints PowerShell/bash env-lines, secrets redacted. See [Set up from the CLI](#set-up-from-the-cli---setup--export-your-config---export-config). |
| `--export-config` | off | — | Print the current effective config (`config.json` + env overrides) as a `config.json` blob plus PowerShell/bash env-lines, then exit. Secrets redacted by default. |
| `--include-secrets` | off | — | With `--export-config`, emit API keys in full instead of `***` (for backup/migration). |
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
& "$env:USERPROFILE\whisper-dictate-venv\Scripts\python.exe" -m pip install `
  -r "$env:LOCALAPPDATA\Programs\WhisperDictate\requirements\parakeet.txt"
setx VOICEPI_STT_BACKEND parakeet
```

(Older installs may have `voice-pi-venv` instead of `whisper-dictate-venv`; both
names keep working — the runtime prefers `whisper-dictate-venv` and only falls back to the legacy `voice-pi-venv` when the new directory is absent.)

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

### Benchmark corpus

The **System** tab's **Run benchmark** button runs a "golden corpus" of
reference sentences through your currently-configured backend and reports a
concise `[benchmark] …` summary (pass count + average WER/CER) in the runtime
log. It needs no arguments.

**Where the manifest is found.** The corpus _manifest_ (`corpus.json`, the
reference text + technical terms — no audio) is resolved in this order:

1. an explicit `--benchmark-corpus PATH` argument, if you pass one;
2. `<app-root>/benchmark/corpus.json` — the dev-checkout layout, and the file
   the installer/ZIP now ship, so the button works out of the box;
3. `%APPDATA%\WhisperDictate\benchmark\corpus.json` on Windows or
   `${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/benchmark/corpus.json`
   elsewhere — a manifest you manage yourself that survives reinstalls.

If no corpus is found anywhere, the log shows one clear line
(`[benchmark] no corpus manifest found (looked: …) — see docs`) and the run
ends cleanly — the button never silently does nothing.

**Audio recordings are yours and stay local.** The manifest references one audio
recording per item, but those `.wav` files are _not_ shipped (they are
user-local and gitignored). For each item, the worker first looks for the
recording next to the manifest, then falls back to the per-user audio dir:

- Windows: `%APPDATA%\WhisperDictate\benchmark\audio\<id>.wav`
- Linux/macOS: `${XDG_CONFIG_HOME:-~/.config}/whisper-dictate/benchmark/audio/<id>.wav`

Keeping recordings there means they survive reinstalls. Items whose audio is
missing everywhere are reported as `skipped` in the summary, so a fresh install
shows e.g. `[benchmark] 0/31 passed, 31 skipped (no audio)` — and when _every_
item is skipped for missing audio, the line appends
`record corpus audio to <that audio dir>` so you know exactly what to do next.
In a dev checkout, `scripts/benchmark/record-corpus.py` records each item next to
the manifest (`benchmark/audio/`); to keep recordings across reinstalls, copy
them into the per-user audio dir above (the worker checks it as a fallback).

### Target profiles

**What they solve:** the best settings aren't the same in every app. You might
want English and clipboard-paste injection in your code editor, Danish in your
email client, and a "prompt" post-processing mode when dictating to a coding
agent. Profiles let you override settings *per target window* without constantly
changing the global config: when the focused window matches a profile, that
profile's `settings` are layered on top of your base config for that utterance,
then drop away when you switch to another window.

**Getting started:** open the **Profiles** tab in the app — it ships with one
inert example profile showing the structure. Edit its `match` to point at a real
app (and tweak the `settings`), then **Save**. The active profile is logged as
`[profile] active: …` and included in metrics/history events, so you can confirm
it fired.

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

**Picking a target window without hand-typing:** in the Profiles tab, click
**List open windows** to fetch the live list of visible windows from the running
OS. The picker shows each window as `process — title`; clicking **Insert**
appends a ready-to-use profile object (pre-filled `match.process` and
`match.title`) to the Profiles JSON above. Edit the `settings` block and
**Save**. Window enumeration is **Windows only** for now — Wayland cannot
enumerate windows without a compositor extension, and X11 support is deferred.
On non-Windows platforms the button reports the limitation in the runtime log
and leaves the JSON untouched. The same enumeration is also available from the
command line:

```powershell
whisper-dictate run --list-windows
```

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

### Fast or rapid speech

When you speak quickly, words run together and the two most common failures are
(1) the voice-activity gate splitting or ending a phrase too early, and (2) the
decoder dropping or merging words. Tune in this order — change one thing, test,
keep what helps:

| Symptom | Setting | Try | Why |
|---|---|---|---|
| Phrase cut off / split mid-sentence | `VOICEPI_VAD_MIN_SILENCE_MS` | `900`–`1200` (default `600`) | Require a longer pause before VAD ends/splits speech, so a quick breath isn't treated as the end |
| First/last syllables clipped | `VOICEPI_VAD_SPEECH_PAD_MS` | `300` (default `200`) | Keep more audio around detected speech |
| Quiet/fast onsets missed | `VOICEPI_VAD_THRESHOLD` | `0.2` (default `0.3`) | More sensitive speech detection (raise again if it triggers on noise) |
| Last word dropped on release | `VOICEPI_RELEASE_TAIL_MS` | `300`–`400` (default `200`) | Capture a bit more after you release the key |
| Words merged / wrong on hard audio | `VOICEPI_BEAM_SIZE` | `5` (default `1`) | Wider beam search = more accurate, slower (cheap on GPU) |
| Long fast sentences lose coherence | `VOICEPI_CONTEXT_MIN_SECONDS` | keep `5` (the default already enables context for ≥5 s utterances) | `condition_on_previous_text` keeps word boundaries coherent |

Model/engine notes for fast speech:

- On a CUDA GPU prefer `VOICEPI_MODEL=large-v3` over `large-v3-turbo` — the full
  model is more robust to fast, slurred or accented speech (turbo trades a little
  accuracy for speed). On CPU, `large-v3-turbo` is the practical default.
- Keep `VOICEPI_TEMPERATURE=0.0` — the fallback ladder can "invent" smoother but
  less faithful text on uncertain (fast) audio.
- `VOICEPI_LANG=<your language>` (not auto-detect) — language detection is
  weaker on the short, run-together clips fast speech produces.
- The local "Skip silent hallucinations" guard (default on) does **not** hurt
  fast speech: it only drops segments the model itself flags as non-speech.
