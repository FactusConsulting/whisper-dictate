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

## Floating status surface

Use the compact-mode button in the top bar to keep a small always-on-top WD
surface visible while working in another application. It shows the current
state (idle, starting, recording, transcribing, injecting, or error), backend,
model, active profile, and microphone level. After an utterance it keeps a
short transcript preview with Copy, Reinject, Retry, Dictionary, and Settings
actions. The surface is session-only and does not change the runtime's focus or
configuration; leave it with the expand button.
On pure Wayland (a `WAYLAND_DISPLAY` without an X11 `DISPLAY`), Copy remains
available but Reinject and Retry are disabled because the compositor does not
provide a portable way to restore the captured target window. Use the original
dictation action or an X11 session for those actions. Reinject and Retry are
available only after the runtime is stopped; click Stop and wait for the status
to return to Stopped before using either action. On macOS, Reinject and Retry
are also unavailable because the platform does not provide target restoration;
use the original dictation action instead.

## Settings reference — every knob at a glance

The full list of runtime settings is generated from the schema below. The
quickest path is the **Desktop Settings UI** (it writes `config.json` for you);
headless users set the matching `VOICEPI_*` env var or `config.json` key. For
copy-paste end-to-end setups jump to the [scenario recipes](#scenario-recipes)
further down.

<!-- BEGIN GENERATED SETTINGS REFERENCE -->
_Generated from `shared/config/settings_schema.json` by `scripts/dev/gen-settings-docs.ps1` -- do not edit this block by hand; regenerate with `pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/dev/gen-settings-docs.ps1`._

Every runtime setting, grouped by area. **Live** settings apply on the next record start/stop; **Restart** settings (backend, model, device, compute type, hotkey) need the worker restarted. The env var is read at startup; the same name without the `VOICEPI_` prefix, lower-cased, is the `config.json` key.

### Core (the first-time-setup basics) -- 7 basic

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `key` | `VOICEPI_KEY` | `pause` | Restart | Hold-to-talk hotkey; pause is the default. Reliable native choices are pause, f1-f12, space, esc, tab, enter, plus generic ctrl/shift/alt/cmd/win modifiers. Side-specific, modifier-only, and multi-trigger chords use the Windows fallback listener; navigation, media, lock, and f13+ names are not supported by every native listener. Letter/digit triggers are Windows-only and are not accepted by the cross-platform UI. |
| `model` | `VOICEPI_MODEL` | `large-v3-turbo` | Restart | Local Whisper model. large-v3-turbo = fastest default; large-v3 = best accuracy, slower. |
| `stt_backend` | `VOICEPI_STT_BACKEND` | `whisper` | Restart | Speech-to-text engine: whisper (local native whisper.cpp) or openai (external OpenAI-compatible cloud API). |
| `device` | `VOICEPI_DEVICE` | `auto` | Restart | Compute device for native local STT: auto uses the compiled GPU backend when available; vulkan explicitly requests the Vulkan backend; cpu disables GPU use. |
| `audio_device` | `VOICEPI_AUDIO_DEVICE` | _(unset)_ | Restart | Microphone/capture device: empty = OS default, an integer device index, or a case-insensitive name substring (e.g. Yeti). Backend-independent. |
| `lang` | `VOICEPI_LANG` | _(unset)_ | Live | Spoken-language hint as an ISO 639-1 code (da, en, de, ...). Empty = auto-detect. Strongly recommended for Whisper. |
| `inject_mode` | `VOICEPI_INJECT_MODE` | `auto` | Live | Text output strategy: auto (type, paste on fragile Windows terminals), type (direct keystrokes), paste (clipboard + paste on X11/Windows), or print (stdout only). |

### Local speech-to-text (Whisper)

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `initial_prompt` | `VOICEPI_INITIAL_PROMPT` | _(unset)_ | Live | Free-text vocabulary/context hint (up to ~1024 chars) biasing recognition toward your domain words and names. |
| `max_chars_per_second` | `VOICEPI_MAX_CHARS_PER_SECOND` | `30` | Live | Speech-rate plausibility gate: drop a transcript whose chars/second exceeds this (0 disables). Real speech is ~15-25 chars/s; impossible rates flag a hallucination. |
| `min_record_seconds` | `VOICEPI_MIN_RECORD_SECONDS` | `0.5` | Live | Discard recordings shorter than this as accidental key taps (effective floor max(0.3, value)), avoiding hallucinated credits on quiet sub-second taps. |
| `preview_seconds` | `VOICEPI_PREVIEW_SECONDS` | `3` | Restart | Local Whisper only: re-transcribe the buffer this often (seconds; 0 disables) so the live Runtime card shows the sentence growing. Display-only. |

### Cloud speech-to-text (OpenAI-compatible APIs)

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `stt_model` | `VOICEPI_STT_MODEL` | _(unset)_ | Restart | External transcription model used only when stt_backend=openai, e.g. gpt-4o-mini-transcribe, gpt-4o-transcribe, whisper-1, or a compatible name. |
| `stt_base_url` | `VOICEPI_STT_BASE_URL` | `https://api.openai.com/v1` | Restart | OpenAI-compatible transcription API base URL, used only when stt_backend=openai (e.g. https://api.groq.com/openai/v1 for Groq). |
| `stt_timeout_ms` | `VOICEPI_STT_TIMEOUT_MS` | `30000` | Restart | Maximum wait (ms) for an external transcription request before it is abandoned. |
| `local_only` | `VOICEPI_LOCAL_ONLY` | _(unset)_ | Restart | Privacy lock: block cloud/BYOK backends and force model libraries into offline mode (HF/Transformers/W&B offline). A library/runtime guard, not an OS firewall rule. |

### Audio capture & voice activity

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `release_tail_ms` | `VOICEPI_RELEASE_TAIL_MS` | `200` | Live | Keep capturing briefly (ms; 0 disables) after the hotkey is released so final syllables/words are not clipped. |
| `max_record_s` | `VOICEPI_MAX_RECORD_S` | `120` | Live | Maximum recording length (seconds; 0 disables the cap). Beyond it, further audio is dropped with a warning; audio up to the cap is still transcribed. |
| `target_dbfs` | `VOICEPI_TARGET_DBFS` | `-20` | Live | Loudness target (dBFS, <= 0) for quiet-boost normalisation. Lower (e.g. -16) boosts quiet speech harder. |
| `min_input_dbfs` | `VOICEPI_MIN_INPUT_DBFS` | `-55` | Live | Reject utterances quieter than this (dBFS) as 'input too quiet'. |
| `min_snr_db` | `VOICEPI_MIN_SNR_DB` | `6` | Live | Reject utterances with speech-vs-noise contrast below this (dB) as 'no speech contrast'. |
| `audio_ducking` | `VOICEPI_AUDIO_DUCKING` | _(unset)_ | Restart | Windows only: while recording, lower other apps' audio sessions and restore them before transcription. Disabled by default. |
| `audio_ducking_level` | `VOICEPI_AUDIO_DUCKING_LEVEL` | `0.25` | Restart | Target volume (0.0-1.0) for other apps' audio while recording when audio ducking is enabled. |

### Dictionary & post-processing

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `format_commands` | `VOICEPI_FORMAT_COMMANDS` | `off` | Live | Opt-in deterministic spoken formatting commands: off, en (new line/comma/period), da (ny linje/komma/punktum), or both. |
| `dictionary` | `VOICEPI_DICTIONARY` | _(unset)_ | Live | Path(s) to custom dictionary file(s) providing a bounded vocabulary prompt plus exact smart replacements for product/app names. |
| `dictionary_enabled` | `VOICEPI_DICTIONARY_ENABLED` | `1` | Live | Toggle dictionary loading without removing the file. Set 0/false/no/off to disable. |
| `dictionary_max_terms` | `VOICEPI_DICTIONARY_MAX_TERMS` | `80` | Live | Maximum number of dictionary terms appended to the Whisper prompt, keeping prompt injection bounded as the dictionary grows. |
| `dictionary_prompt_chars` | `VOICEPI_DICTIONARY_PROMPT_CHARS` | `1200` | Live | Maximum total characters used by dictionary terms in the prompt. |
| `post_processor` | `VOICEPI_POST_PROCESSOR` | `none` | Restart | Optional second text pass after STT and dictionary replacements: none, ollama (local), or openai/groq (cloud, blocked by local_only). |
| `post_mode` | `VOICEPI_POST_MODE` | `raw` | Live | Rewrite style for the post-processor: raw, clean, prompt, terminal, slack, email, or bullets (bullet-list alias). |
| `post_model` | `VOICEPI_POST_MODEL` | `qwen2.5:3b` | Live | Text model used by the selected post-processor: an Ollama model name or an OpenAI-compatible chat model. |
| `post_base_url` | `VOICEPI_POST_BASE_URL` | `http://localhost:11434` | Restart | Post-processing endpoint (local Ollama by default; an OpenAI-compatible /chat/completions URL for cloud). Blocked for external providers by local_only. |
| `post_timeout_ms` | `VOICEPI_POST_TIMEOUT_MS` | `4000` | Live | Base/floor wall-clock budget (ms) for the rewrite; the effective timeout scales with length (+20 ms/char up to a 30 s ceiling) then falls back to dictionary-final text. |
| `post_max_input_chars` | `VOICEPI_POST_MAX_INPUT_CHARS` | `4000` | Live | Maximum number of characters sent to the post-processor. |
| `post_max_output_chars` | `VOICEPI_POST_MAX_OUTPUT_CHARS` | `4000` | Live | Maximum number of rewritten characters accepted back from the post-processor. |
| `post_redact` | `VOICEPI_POST_REDACT` | _(unset)_ | Live | Opt-in local redaction before cloud post-processing: replace emails, phone numbers and common API tokens with placeholders, restored afterward when possible. |
| `post_redact_terms` | `VOICEPI_POST_REDACT_TERMS` | _(unset)_ | Live | Extra comma-separated names/terms to redact before cloud post-processing. Original values are never written to metrics. |

### Injection, hotkeys & feedback

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `xkb_layout` | `VOICEPI_XKB_LAYOUT` | _(unset)_ | Restart | Wayland only: force the keycode layout for special-character injection (dk, se, de, fi, no, ...), overriding auto-detection. |
| `feedback_sounds` | `VOICEPI_FEEDBACK_SOUNDS` | _(unset)_ | Live | Play a short audio cue on record start/stop, useful when the console is hidden (headless/autostart). Non-blocking. |
| `toggle_mode` | `VOICEPI_TOGGLE` | _(unset)_ | Restart | Toggle mode: press the hotkey to start recording, press again to stop and transcribe, instead of holding it. Restart-only. |

### Diagnostics, history & automation

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `json_output` | `VOICEPI_JSON` | _(unset)_ | Restart | Print one structured JSON event per accepted utterance to stdout; also gates the metrics file (metrics_jsonl is only written while this is on). |
| `metrics_jsonl` | `VOICEPI_METRICS_JSONL` | _(unset)_ | Restart | Append one structured JSON event per accepted utterance to this file. Only written while JSON output is enabled. |
| `command_hook` | `VOICEPI_COMMAND_HOOK` | _(unset)_ | Live | Advanced opt-in automation hook (command string or JSON array). Receives one utterance JSON event on stdin and runs without shell interpolation. |
| `command_hook_timeout_ms` | `VOICEPI_COMMAND_HOOK_TIMEOUT_MS` | `2000` | Live | Maximum wait (ms) for the command hook. Timeout/failure is logged and recorded but does not block injection. |
| `history_enabled` | `VOICEPI_HISTORY_ENABLED` | `1` | Restart | Store accepted live dictations locally for copy/reinject/debug recovery. Set 0/false/no/off to disable. |
| `history_jsonl` | `VOICEPI_HISTORY_JSONL` | _(unset)_ | Restart | Override the local history JSONL path (default under the per-user state dir). |
| `log_level` | `VOICEPI_LOG` | `info` | Live | Native diagnostic verbosity: off, info (lifecycle), debug (runtime decisions and action flow), or trace (high-volume input, environment-key, and teardown flow). |

### Update checks

| Key | Env var | Default | Live/Restart | Description |
|---|---|---|---|---|
| `update_check` | `VOICEPI_UPDATE_CHECK` | `1` | Live | UI only: periodically check the public GitHub version list and show a discreet 'update available' badge. Sends no data/telemetry; skipped when local_only is on. |
| `update_check_interval_minutes` | `VOICEPI_UPDATE_CHECK_INTERVAL_MINUTES` | `15` | Live | UI only: how often (minutes, clamped to >= 5) the in-app update check polls the public version list. |
| `update_include_prereleases` | `VOICEPI_UPDATE_INCLUDE_PRERELEASES` | `0` | Live | UI only: opt in to update notifications for release candidates (pre-releases), not just final releases. Off by default; live on the next poll. |
<!-- END GENERATED SETTINGS REFERENCE -->

### Settings not in the schema

A few `VOICEPI_*` env vars and `config.json` keys are intentionally _not_ in
`settings_schema.json` (they are secrets, UI-only knobs, or rarely-tuned
advanced guards) and so are documented by hand here:

| Variable / key | Default | Values | Effect |
|---|---|---|---|
| `VOICEPI_STT_API_KEY` / `GROQ_API_KEY` / `OPENAI_API_KEY` | _(unset)_ | API key | Bearer token for `stt_backend=openai`. `VOICEPI_STT_API_KEY` wins; `GROQ_API_KEY` is used when the base URL points at Groq; `OPENAI_API_KEY` is the generic fallback. The Rust UI stores provider keys in the **OS credential store** and passes them to the worker as `VOICEPI_STT_API_KEY`; headless runs use the env var. **Never** stored in `config.json`. |
| `VOICEPI_POST_API_KEY` / `GROQ_API_KEY` / `OPENAI_API_KEY` | _(unset)_ | API key | Bearer token for cloud post-processing. `VOICEPI_POST_API_KEY` takes precedence; otherwise the worker can fall back to the loaded Cloud STT key. |
| `stt_provider` (`config.json`) | `openai` | `openai` \| `groq` | Rust UI cloud-STT provider selector. Sets `VOICEPI_STT_BASE_URL` and provider-specific model choices for the managed worker; existing Groq-URL configs are migrated to `groq`. |
| `ui_theme` (`config.json`) | `dark` | `dark` \| `light` | Rust settings UI visual theme. UI-only; does not restart dictation or affect the native runtime. |
| `XKB_DEFAULT_LAYOUT` | _(unset)_ | XKB layout name | **Wayland only.** Consulted after `VOICEPI_XKB_LAYOUT` for special-char injection layout; `--lang` auto-sets it if unset. |
| `VOICEPI_NO_COLOR` / `NO_COLOR` | _(unset)_ | any non-empty | Disable ANSI styling for interactive terminal status lines. Piped output, logs, JSON and the Rust UI stay plain automatically. |
| `VOICEPI_SKIP_SYSCHECK` | _(unset)_ | any non-empty | Linux: skip the `packaging/linux/ubuntu26.04/setup.sh` apt-dep check. Auto-set by the Homebrew/Nix wrappers. |

See [MICROPHONE.md](MICROPHONE.md) for what the capture-tuning dBFS/SNR
numbers mean in practice.

## Scenario recipes

Ready-to-paste, CLI/headless-focused setups for the four most common
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
`device`, `audio_device`, `key`) need the runtime restarted; the rest apply on
the next record start/stop.

### Set up from the CLI / export your config

If you do not want to hand-write `config.json`, the Rust controller can build it
for you and dump an existing one. These commands do not load a speech model or
require the desktop UI.

- **`whisper-dictate setup`** launches an interactive wizard driven by the
  settings schema. It walks the **basic** first-setup knobs first (showing each
  setting's description, its current value or default, and the valid choices for
  enum settings like `stt_backend`/`device`/`inject_mode`), then asks
  **`Run advanced setup? [y/N]`** before walking the rest grouped by category.
  Press ENTER to keep the shown value; type to change it; numeric settings are
  re-prompted if you exceed the schema's min/max bounds. On finish it writes
  `config.json` to the standard path (printed) with only non-default values,
  preserving existing advanced overrides even if advanced setup is skipped.
  It then prints equivalent PowerShell (`$env:VOICEPI_X = '...'`) and bash
  (`export VOICEPI_X=...`) lines. API keys remain in the OS credential store or
  environment and are never written to `config.json`. With non-interactive
  stdin, the wizard reads scripted answers line by line instead of hanging.

- **`whisper-dictate export-config`** prints your **current effective
  config** — `config.json` merged with any `VOICEPI_*` env overrides, resolved
  exactly the way the runtime resolves settings at startup — as a `config.json`
  blob plus ready-to-paste PowerShell and bash env-lines. Secrets are
  collected from the environment and OS credential store and **redacted by
  default**; add **`--include-secrets`** to emit them in full for an explicit
  backup/migration operation.

### Recipe A — Local STT on GPU (Whisper)

Run everything locally with the vendor-neutral Vulkan GPU backend. No network, no keys. See the
[GPU VRAM sizing](#gpu-vram-sizing--what-to-set-per-card) guidance to pick a
model for your free VRAM.

`config.json`:

```json
{
  "stt_backend": "whisper",
  "model": "large-v3",
  "device": "vulkan",
  "lang": "da"
}
```

PowerShell (persistent env, honoured by the Start-menu shortcut):

```powershell
setx VOICEPI_STT_BACKEND whisper
setx VOICEPI_MODEL large-v3
setx VOICEPI_DEVICE vulkan
setx VOICEPI_LANG da
# restart whisper-dictate so the new process inherits these
```

bash:

```bash
export VOICEPI_STT_BACKEND=whisper
export VOICEPI_MODEL=large-v3
export VOICEPI_DEVICE=vulkan
export VOICEPI_LANG=da
```

Notes:

- **VRAM:** numeric precision is determined by the whisper.cpp model file, not
  a runtime setting. Run `whisper-dictate model-capacity` before loading; if
  the first transcription runs out of memory, choose a smaller model.

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
- **Language:** the rewrite prompt names the language the transcription pass
  actually ran in and instructs the model to reply in that same language, never
  to translate, and to leave numerals exactly as dictated. That is the
  _effective_ language for each utterance — a `--lang` flag or a per-application
  profile's `lang` wins over the saved setting, and on auto-detect it is the
  language the model detected. When nothing is known (auto-detect, nothing
  detected) the prompt still binds the reply to the language of the input, so an
  unset language never licenses a translation either.

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
  "feedback_sounds": "1"
}
```

bash (e.g. in `~/.profile`, a systemd user unit, or the autostart `.desktop`
environment):

```bash
export VOICEPI_INJECT_MODE=auto
export VOICEPI_XKB_LAYOUT=dk        # force the keycode layout for æ ø å etc.
export VOICEPI_LANG=da
export VOICEPI_FEEDBACK_SOUNDS=1    # audible cue since no console is visible
```

Notes:

- **Wayland injection uses `ydotool`** (direct evdev keycodes); the daemon needs
  access to `/dev/uinput`. On NixOS the module already wires up ydotool/uinput;
  on other distros install `ydotool`, run `ydotoold`, and add your user to the
  `input` group (or grant uinput access). Run `whisper-dictate doctor` to
  check the Wayland health prerequisites before loading Whisper.
- **`xkb_layout`** sets the keycode layout for special-character injection
  (highest priority; `XKB_DEFAULT_LAYOUT` is the fallback, and `--lang`
  auto-sets it if unset). Use `dk`, `se`, `de`, `no`, … to match your physical
  layout so `æ ø å` and friends land correctly.
- **X11 instead of Wayland:** `inject_mode` `type`/`paste` are both supported by
  the native injector. Stop the managed runtime explicitly from the controller.
- **What a headless server needs:** a working microphone (`whisper-dictate
  devices` and `whisper-dictate devices test "<name>"` to verify
  without loading a model), the injection backend above, and
  `feedback_sounds` for recording cues. Keep the native runtime log available
  for actionable model, capture, and injection errors. There is no separate
  "server mode"; it is the normal `wd run` launched without a
  terminal (`Terminal=false` in the `.desktop` entry).

### Native hotkey support

The settings field reports syntax and native capability separately. These are
the reliable choices shared by the normal Rust listeners:

| Category | Supported names |
|---|---|
| Modifiers | `ctrl`, `shift`, `alt`, `cmd`/`win`; side-specific `ctrl_l/r`, `shift_l/r`, `alt_l/r`/`alt_gr`, `cmd_l/r`/`win_l/r` are available through the low-level listener. |
| Triggers | `pause`, `f1`–`f12`, `space`, `esc`, `tab`, `enter` |

Other names may still be accepted as configuration tokens for compatibility,
but the UI marks navigation, media, lock, and `f13+` keys as unsupported by
every native listener. Letter and digit triggers are Windows-only and are not
accepted by the cross-platform UI. On Windows
the GUI uses `RegisterHotKey`, which requires one generic modifier set and
exactly one trigger. Side-specific, modifier-only,
or multi-trigger chords use the low-level fallback and may not receive events
while WhisperDictate is the focused window. Use a generic chord such as
`ctrl+f9` for the reliable Windows path, or run the probe below to verify a
specific physical key on the current machine.

All physical keys cannot be made equally reliable across operating systems:
some are not exposed by the Rust event library, some are consumed by firmware
or desktop software, and Windows global registration has the limits above.

In the Speech settings page, **Capture shortcut** records the supported key
events delivered to the window, shows the canonical chord, and requires an
explicit **Apply** before the value changes. For a global capture session from
a terminal, run `wd hotkey capture --configure --driver auto`; release the
chord, then answer `y` when the command asks to save it. The command uses the
same side-specific modifier names as the UI (`ctrl_l`, `ctrl_r`, and so on)
and never saves without that confirmation.

### Probing a hotkey before you commit — `scripts/dev/probe-key.ps1`

Before `setx VOICEPI_KEY <something>`, verify your OS actually delivers
that key to the native Rust listener. The repo ships a PowerShell wrapper
around the Rust hotkey probe; it requires PowerShell and a Rust toolchain
(`cargo`) but no Python runtime:

```powershell
# Clone or cd into the repo, then:
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/dev/probe-key.ps1 -Chord pause -Duration 15
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/dev/probe-key.ps1 -Chord ctrl_r+space
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/dev/probe-key.ps1                 # passive: log EVERY key event
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/dev/probe-key.ps1 -Chord f9 -Duration 30
```

Common gotchas the probe catches:

- **Pause/Break missing on tenkeyless / laptop keyboards** — no physical
  Pause key, nothing to trigger.
- **Pause intercepted by gaming-keyboard firmware** (Razer/Corsair) —
  swallowed before the native listener sees it.
- **`caps_lock` state-toggle on Windows** — press fires once, release
  doesn't fire on hold; breaks the hold-to-talk model.
- **Multimedia keys eaten by OEM software** before reaching the native listener.
- **Chord like `ctrl_r+space` filtered by IME / IntelliSense** in some
  apps.

Exit codes: `0` = chord verified, `1` = no events at all (OS not
 delivering), `2` = events arrived but the full chord was never held
 together, `3` = unknown or unsupported key name. Passive mode returns
 `0` after a successful listener run.

### Native diagnostics levels

The UI **Diagnostics** dropdown now writes the native logger's
`VOICEPI_LOG` setting directly:

- **Off** (`off`): no diagnostic-file output.
- **Basic** (`info`, the default): lifecycle, readiness, and terminal failures.
- **Verbose** (`debug`): Basic plus runtime decisions, settings reloads,
  coordinator actions, capture/injection stages, and suppression reasons.
- **Trace** (`trace`): Verbose plus high-volume input, environment-key,
  capture, and teardown flow.

Changes apply at the next utterance boundary. Debug and trace lines deliberately
log setting names and state transitions rather than prompt, hook, or credential
values. Trace is high volume; return to Basic after troubleshooting.

<a id="privacy-warning-debug-trace-logs-capture-global-keystroke-activity"></a>

> **Privacy warning — debug/trace logs capture global keystroke activity.**
> When the Rust hotkey backend is active (`whisper-rs-hotkeys`, the default
> on Windows from v1.22.0 onward), the LL-hook callback observes **every
> desktop-wide keydown/keyup** — including keys you press outside
> whisper-dictate. At **Verbose**/**Trace** the hotkey diagnostic path may
> record `[hotkey/rdev]` and `[chord]` samples that trace event counts,
> presses/releases and, for PTT-eligible keys, key names. Passwords, tokens
> and other sensitive text you type into other apps could therefore be
> reconstructable from a Verbose/Trace log covering that window.
>
> The redaction added in the sweep for Codex #646 (and extended by
> the sweep for #665) replaces non-PTT key names with `<redacted>`
> in both the `[hotkey/rdev] raw event` lines AND the tracker's
> `[chord]` line, so at **`VOICEPI_LOG=debug`** ordinary typing no
> longer leaves its literal key identity in the log. Metadata
> (timing, event counts, Press/Release) is still recorded.
>
> **At `VOICEPI_LOG=trace` the redaction is NOT sufficient on its
> own.** The parallel Windows `WH_KEYBOARD_LL` diagnostic hook
> (`[win/raw-hook]` lines, Windows only, only enabled at `trace`)
> records the raw `vkCode` and scan code of every sampled desktop
> keystroke — those values directly identify ordinary keys and can
> reconstruct passwords or tokens typed anywhere while the log is
> capturing. If you must run at `trace` for a Windows PTT-drop
> investigation, treat the resulting log as sensitive and share it
> privately with the maintainers rather than on a public issue.
>
> **Before attaching a Verbose/Trace log to a bug report:**
>
> - keep the capture window as short as possible (only cover the
>   reproduction),
> - stop Verbose/Trace before typing anything sensitive,
> - open the log file and skim `[hotkey/*]`, `[chord]`, `[stt]`,
>   `[stt-debug]` lines for anything you would not want to share, and
> - if in doubt, redact or share the log privately with the maintainers
>   instead of on a public issue.
>
> The **Diagnostics** choice and `VOICEPI_LOG` are now the same native gate.
> Choose Off to stop `[hotkey/rdev]`, `[chord]`, and other native lines
> (including the `t=<ms> [gui] starting …` startup marker). See the
> [Diagnostic env vars — `VOICEPI_LOG`](#diagnostic-env-vars--voicepi_log)
> section below for the full level table.

### Debugging "is my `setx` arriving?" — Verbose diagnostics

A common confusion on Windows is that `setx` writes to the user registry,
but **only new processes inherit it** — a whisper-dictate launched from a
stale Start-menu shortcut or tray-restart may still see the old values.

To verify what the running process actually sees, set **Verbose**
diagnostics (`VOICEPI_LOG=debug`) and restart. Native diagnostics report the
resolved backend/device choices and applied environment-key names without
printing secret values:

```text
[runtime/debug] start stage=apply-worker-config
[runtime/trace] applied session env key=VOICEPI_LANG
[runtime/trace] effective options device=vulkan stt_backend=whisper
  inject mode        auto  (env VOICEPI_INJECT_MODE=(unset))
loading Whisper large-v3 with Vulkan…
```

If a value shows `(unset)` where you expected one, your `setx` didn't
reach this process — log out + back in, or launch from a fresh PowerShell
where `$env:VOICEPI_X` shows the value. Set Diagnostics to **Off** for
normal use; **Basic** if you want the lightweight per-utterance `[health]`
line; **Verbose** adds the config dump (~10 lines on startup) and per-segment
detail; **Trace** adds the full audio-device enumeration and a line per
capture-open attempt (for diagnosing a mic that won't open). All have zero
runtime cost when their level is not selected.

### Diagnostic env vars — `VOICEPI_LOG`

For hotkey investigations (a PTT press that seems to reach the OS but
never triggers a recording), Windows GUI builds tee a diagnostic log
to `%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log`. The volume of
that log is controlled by a single env var whose name and level
values mirror the Rust ecosystem's standard (`RUST_LOG`, `log`,
`tracing`):

| `VOICEPI_LOG` | What it enables | When to use |
|---|---|---|
| `off` | Nothing — not even startup markers. | You need the tee file to stop growing entirely. |
| `error` | Only errors that stopped something working. | Silent operation with a paper trail if something breaks. |
| `warn` | Errors we recovered from (fallback branches, Phase-B degraded, ...). | Same, plus recovered-but-worth-knowing conditions. |
| `info` (default) | Startup markers, rdev listener heartbeat (~one line every 5 s), rate-limited per-event trace (first ten events, then every 100th), session-start / stop events. | Release default. Matches what shipped in PR #646 — existing users see no change. |
| `debug` | Everything in `info` PLUS the rdev-boundary trace for EVERY event rdev delivers, chord-matcher trace, coordinator state-transition trace, and session-dispatch refuse/emit trace. | Active investigation when the info-level heartbeat can't pinpoint the wedge. Adds ~1-5 KB / minute of typing. |
| `trace` | Everything in `debug` PLUS the parallel Windows `WH_KEYBOARD_LL` hook that dumps every desktop-wide key event (WM message, vk, scan, flags, injected/extended). | Deep investigation of a key that debug-level can't see on either the rdev callback OR the tracker. High volume — 500+ lines/minute during typing. Turn back to `info` when you're done. |

Accepted aliases: `0` / `false` / `no` → `off`; `1` / `true` / `yes`
/ `on` → `info`; `err` → `error`; `warning` → `warn`; `dbg` →
`debug`; `verbose` / `all` / `full` → `trace`. Case-insensitive;
whitespace is trimmed. An unknown value falls back to `info` with a
one-line warning at the top of the log so a typo is visible rather
than silently downgraded.

The level is captured once at GUI startup and cached in an atomic —
changing the env var after launch has no effect until the next
process start.

The startup marker line records the resolved level so the reader
knows what to expect:

```text
t=12ms [gui] whisper-dictate-gui 1.22.0-rc.11 starting; VOICEPI_LOG=trace; diagnostic log at C:\Users\…\gui-diagnostic.log
```

**Trace line prefixes** (grep-friendly, stable across releases):

| Prefix | Layer | Level threshold |
|---|---|---|
| `[gui]` | GUI startup / lifecycle | `error` (always visible unless `off`) |
| `[hotkey]` / `[runtime]` | Hotkey install path, supervisor Phase-B branches | `error`-`info` depending on message |
| `[hotkey/rdev]` | rdev listener heartbeat + rate-limited per-event trace (PR #646) | `info` |
| `[rdev/callback]` | Every event rdev's own callback fires, BEFORE the name-filter | `debug` |
| `[chord]` | Chord matcher — result for every raw event | `debug` |
| `[coord]` | Coordinator state transitions | `debug` |
| `[dispatch]` | Session dispatch (start / stop / cancel emitted or refused) | `debug` |
| `[win/raw-hook]` | Parallel `WH_KEYBOARD_LL` hook (Windows only) | `trace` |

**Before sending a diagnostic log to us — please read the privacy
warning above.** A `debug`- or `trace`-level log covers every desktop-wide
keystroke that happened during the capture window: `[rdev/callback]`
runs on every event (with key identity redacted for non-PTT keys, so
your typing does _not_ land verbatim), and `[chord]` records the
sequence of PTT-eligible presses/releases the tracker saw. Set
`VOICEPI_LOG=info` (or `off`) before typing anything sensitive, and
open the file to skim `[hotkey/*]` / `[chord]` / `[stt]` /
`[stt-debug]` lines before attaching. If in doubt, share privately
with the maintainers rather than on a public issue.

**Sending a diagnostic log to us:** once you have skimmed it,
compress `%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log` and
attach it to the bug report. Reset `VOICEPI_LOG=info` (or delete the
file) afterwards — the append-mode tee keeps growing across
sessions.

**Decision tree** when reading a `trace` log:

- F9 press generates `[win/raw-hook]` but no `[rdev/callback]` → rdev
  is silently dropping the event on its own listen() plumbing.
- No `[win/raw-hook]` for F9 either → another program's LL hook
  higher in the chain is consuming F9 before either of our hooks is
  called (Logi G HUB / Options+, NVIDIA Broadcast, antivirus
  keylogger prevention, ...).
- `[rdev/callback]` fires but no `[chord]` line matches → an rdev
  event-boundary bug (regressed `raw_from_rdev` name-filter).
- `[chord]` fires with `match=Some(ChordPress)` (or
  `match=Some(ChordRelease)`) but no `[coord]` line →
  the tracker-to-coordinator mpsc channel is disconnected. (The
  literal field value comes from `Debug` formatting of an
  `Option<TrackerOutput>` in `manager/tracker.rs`, so `match=None`
  is emitted when the tracker suppressed the event — e.g. an OS key
  repeat or a foreign key that broke a bare-modifier chord — and is
  the expected marker for "raw event received, no chord change".)
- `[coord]` shows `Idle-->Recording` but no `[dispatch]
  session_start emitted` → sink mutex poison / listener dropped.

## CLI flags

Passed after the Rust controller (`wd run -- ...`):

| Flag | Default | Values | Effect |
|---|---|---|---|
| `--key` | `$VOICEPI_KEY` or `pause` | native key name, or chord `a+b` | Hold-to-talk key. `pause` is the default. Use a common trigger (`pause`, `f1`–`f12`, `space`, `esc`, `tab`, `enter`) with generic `ctrl`, `shift`, `alt`, `cmd`, or `win`; the UI warns about fallback and unsupported names. |
| `--model NAME` | `$VOICEPI_MODEL` | see `VOICEPI_MODEL` | Whisper model for this run. |
| `--lang CODE` | `$VOICEPI_LANG` | ISO 639-1 code | Force language for this run. Omit to auto-detect. |
| `--autodetect` | off | — | Force language auto-detect (overrides `--lang`/`VOICEPI_LANG`). |
| `--prompt TEXT` | `$VOICEPI_INITIAL_PROMPT` | free text (~1024 chars), or `""` to disable | Domain-vocabulary hint seeded into Whisper's initial prompt for this run, e.g. `--prompt "Kubernetes, Proxmox, LiteLLM, ansible"`. Wins over `VOICEPI_INITIAL_PROMPT` / the saved `initial_prompt` setting and stays authoritative for the whole session (a live config reload won't override it). Pass `--prompt ""` to disable the hint for this run. |
| `--device D` | `$VOICEPI_DEVICE` | `auto` \| `vulkan` \| `cpu` | Compute device for this run. `vulkan` is only honoured by binaries built with `--features whisper-rs-vulkan` (or the GPU installer); on a CPU-only binary the option is refused rather than silently demoting to CPU. Legacy saved `cuda` values migrate to `vulkan`. |
| `--type` | `$VOICEPI_INJECT_MODE` or off | — | Force direct keyboard typing on X11/Windows. (Wayland always uses direct evdev keycodes regardless.) |
| `--paste` | `$VOICEPI_INJECT_MODE` or off | — | Force native clipboard paste: copies text to the system clipboard, then sends the platform paste shortcut (Ctrl+V or Ctrl+Shift+V for terminals). Wayland uses the configured native helper chain; Windows and X11 use their native injection backends. If the previous clipboard could be read, it is restored after a short delay — but only when the clipboard still holds the injected text (your own copy in the meantime is never overwritten). |
| `--no-type` | `$VOICEPI_INJECT_MODE` or off | — | Print the transcription only, don't inject (testing). |
| `--json` | `$VOICEPI_JSON` or off | — | Also print one structured JSON event per accepted utterance. |
| `whisper-dictate doctor` | off | — | Run Linux/Wayland health checks and exit before loading Whisper. |
| `whisper-dictate setup` | off | — | Rust-native interactive config wizard (no model load): derives defaults, choices, and numeric bounds from the shared schema, writes `config.json`, and prints PowerShell/bash env-lines. |
| `whisper-dictate export-config` | off | — | Rust-native effective-config export (`config.json` + environment precedence) as a JSON blob plus correctly quoted PowerShell/bash lines. Secrets from the environment or credential store are redacted by default. |
| `--include-secrets` | off | — | With `whisper-dictate export-config`, emit API keys in full instead of `***` for an explicit backup/migration operation. |
| `whisper-dictate model-capacity` | off | — | Show NVIDIA GPU free/total VRAM and a local model fit table from the Rust controller before loading a model. |
| `whisper-dictate transcribe-file PATH [--json]` | text | 16 kHz mono WAV | Rust-native one-shot transcription in distributions that ship the Rust controller. Configured cloud STT works in every Rust-controller build; local STT requires `whisper-rs-local`, included in shipping release and Nix builds but not a default `cargo run` or the lightweight Linux source installer. Applies the configured language, bounded prompt/dictionary terms, replacements, and post-processing; never falls back to another engine. MP3/M4A/stereo/other sample rates are rejected with an actionable `ffmpeg -i INPUT -ac 1 -ar 16000 OUTPUT.wav` conversion hint. |
| `whisper-dictate bench` | off | — | Run the golden benchmark corpus (`benchmark/corpus.json`) through the configured backend via the native Rust runner and print per-item JSONL plus one `[benchmark]` summary line. Same code path as the System tab's "Run benchmark" button. |
| `--benchmark-corpus PATH` | off | manifest path | Corpus manifest path used by `--dictionary-build-from-corpus` (forwarded to the Rust `dictionary build-from-corpus` subcommand). |
| `whisper-dictate calibrate-mic [SECONDS] [--device NAME] [--json]` | off | seconds, default `5` | Rust-native bounded microphone calibration using the configured device unless overridden. Prints pass/warn/fail audio diagnostics and recommended threshold settings without launching Python. Requires a shipping build with `audio-capture`. |
| `whisper-dictate calibrate-file PATH [--json]` | off | 16 kHz mono WAV | Rust-native file calibration using the same DSP and recommendation logic. Invalid, short, and silent inputs fail clearly; no Python process is launched. |
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
setx VOICEPI_INITIAL_PROMPT "rødgrød med fløde, FactusConsulting, whisper-dictate"
setx VOICEPI_DICTIONARY "%APPDATA%\WhisperDictate\dictionary.json"
setx VOICEPI_MODEL large-v3
setx VOICEPI_DEVICE vulkan
setx VOICEPI_KEY "ctrl_l+space"
setx VOICEPI_INJECT_MODE auto
# then restart whisper-dictate (new process picks them up)
```

One-off via terminal (the installer put the Rust controller on PATH):

```powershell
& "$env:LOCALAPPDATA\Programs\WhisperDictate\whisper-dictate.exe" run --key ctrl_r --lang da --model large-v3 --device auto
```

Or make your **own** shortcut whose Target is
`%LOCALAPPDATA%\Programs\WhisperDictate\whisper-dictate.exe run --key ctrl_r --lang da`

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

### Self-host the STT model in a container

Only the heavy Whisper model can be containerized. The desktop app still runs on
your machine because microphone capture, global push-to-talk, and text injection
are desktop integrations.

Run an OpenAI-compatible transcription server and point whisper-dictate at it:

```powershell
docker compose -f packaging/docker/docker-compose.yml up -d
setx VOICEPI_STT_BACKEND openai
setx VOICEPI_STT_BASE_URL http://localhost:8000/v1
setx VOICEPI_STT_MODEL Systran/faster-whisper-large-v3
```

In the Rust UI Speech tab choose `Speech engine = Cloud STT`,
`Cloud STT provider = Custom (OpenAI-compatible)`, set the API URL to
`http://localhost:8000/v1`, and set the model name expected by the server. A
loopback URL is allowed even when `VOICEPI_LOCAL_ONLY=1` because audio stays on
the machine.

### Desktop settings UI

On Windows, the installer adds a Start-menu **whisper-dictate** shortcut which
opens a unified control UI: it starts/stops dictation, shows the runtime log,
saves settings and restarts dictation automatically when a restart-only setting
changes. On Ubuntu with Homebrew, run the bundled setup once; it creates a
GNOME **Whisper Dictate** app launcher, login autostart entry, and opens the UI:

```bash
wd setup-ubuntu
```

After that, open **Whisper Dictate** from the Ubuntu launcher or run:

```bash
wd ui
```

For source or release-zip installs, install the Rust UI/controller manually:

```bash
scripts/linux/install-rust-ui.sh
wd ui
```

The Rust UI edits `%APPDATA%\WhisperDictate\config.json`, can create/open the
dictionary file, previews dictionary prompt terms, and restarts its managed
dictation process when restart-only settings change. A running standalone
dictation process also applies live-safe changes on the next record start/stop:
language, inject mode, dictionary, audio thresholds, prompt, JSON/metrics
and the native diagnostic log level. Backend, model, device, compute type and hotkey are
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
whisper-dictate dictionary suggest-replacements benchmark\results.jsonl
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
Record missing items with `whisper-dictate corpus-record <ID>` (the native
Rust recorder that replaced the previous `scripts/benchmark/record-corpus.py`
dev script). Recordings land in the per-user audio dir above by default, so
they survive reinstalls automatically.

### Target profiles

**What they solve:** the best settings aren't the same in every app. You might
want English and clipboard-paste injection in your code editor, Danish in your
email client, and a "prompt" post-processing mode when dictating to a coding
agent. Profiles let you override settings _per target window_ without constantly
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
whisper-dictate list-windows
```

### Injection smoke test

To test a target app without loading Whisper, focus the input field and run:

```powershell
sleep 3; whisper-dictate inject-text "hello world - spaces stay intact, ae oe aa, 123." --do-it --backend auto
sleep 3; whisper-dictate inject-text "hello world - spaces stay intact, ae oe aa, 123." --do-it --backend type
sleep 3; whisper-dictate inject-text "hello world - spaces stay intact, ae oe aa, 123." --do-it --backend paste
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
# new shell, then:
wd run --key shift_r+ctrl_r --lang da
```

Or inline for one run:

```bash
VOICEPI_LANG=da wd run --key shift_r+ctrl_r
```

### Linux — manual Rust controller

Install the Rust controller, then use env vars or flags:

```bash
scripts/linux/install-rust-ui.sh
VOICEPI_LANG=da wd run --key ctrl_r --lang da
```

### NixOS / Nix

`nix run` — env before the command, flags after `--`:

```bash
VOICEPI_LANG=da \
  nix run github:FactusConsulting/whisper-dictate -- run --key shift_r+ctrl_r --lang da
```

NixOS module — set env in the service/user environment (e.g.
`environment.sessionVariables.VOICEPI_LANG = "da";`) and the wrapper inherits
it. `VOICEPI_XKB_LAYOUT` is auto-derived from `--lang`/the session layout; the
module already wires up ydotool/uinput for Wayland.

## GPU VRAM sizing — what to set per card

Run `whisper-dictate model-capacity` to inspect local NVIDIA GPU free/total
VRAM and get a model-fit table for Whisper and local Ollama post-processing
models. On Windows, the Settings UI exposes the same check on the Core tab as
**Model fit**.

Pick the row matching your **free** VRAM (run `nvidia-smi --query-gpu=memory.free
--format=csv` — browser/IDE/Discord eat 1–3 GB before whisper-dictate starts,
so free ≠ total). Round down to the nearest row. If the first transcription
OOMs, choose a smaller model. Numeric precision and quantisation are properties
of the downloaded whisper.cpp model file; there are no separate runtime
precision or beam-size settings.

**One-liner to set the 8–12 GB row** (RTX 3080 / 4070):

```powershell
setx VOICEPI_DEVICE vulkan; setx VOICEPI_MODEL large-v3; setx VOICEPI_LANG da
# restart whisper-dictate; the first [whisper] line reports the resolved accelerator
```

## Native transcription and GPU acceleration

The native runtime runs whisper.cpp inside the Rust binary. GPU support is a
compile-time feature. `VOICEPI_DEVICE` selects `auto`, `vulkan`, or `cpu`;
`vulkan` is strict and a CPU-only build rejects it with an actionable
capability error. `VOICEPI_WHISPER_GPU` selects `auto`, `vulkan`, or `off` and
is a best-effort policy: on a CPU-only build, `vulkan` remains usable by
falling back to CPU. The diagnostic log records the resolved accelerator.

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

When you speak quickly, words can run together or the release boundary can clip
the last word. The native capture route is VAD-free, so tune the release tail,
model, language, and microphone levels:

| Symptom | Setting | Try | Why |
|---|---|---|---|
| Last word dropped on release | `VOICEPI_RELEASE_TAIL_MS` | `300`–`400` (default `200`) | Capture a bit more after you release the key |

Model/engine notes for fast speech:

- With Vulkan GPU acceleration prefer `VOICEPI_MODEL=large-v3` over `large-v3-turbo` — the full
  model is more robust to fast, slurred or accented speech (turbo trades a little
  accuracy for speed). On CPU, `large-v3-turbo` is the practical default.
- `VOICEPI_LANG=<your language>` (not auto-detect) — language detection is
  weaker on the short, run-together clips fast speech produces.
