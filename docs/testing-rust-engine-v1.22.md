# Testing v1.22.0-rc.1: opt-in Rust dictation engine

## What this is

- **v1.22.0-rc.1 is a PRERELEASE.** It ships on the prerelease channels
  (Chocolatey `--prerelease`, GitHub Releases). Homebrew and winget stay
  on the last final release until v1.22.0 ships.
- It ships the **opt-in Rust dictation runtime** as Phase A of
  [audit item 5](./design/item5-wire-dictate-session.md) — the Rust
  `DictateSession` plus real backends now runs behind a subprocess when
  you opt in.
- **Default behaviour is UNCHANGED.** If you install v1.22.0-rc.1 and do
  not set the opt-in env var, you get the exact Python push-to-talk loop
  you had on v1.21.x, byte-for-byte.

## Install

### Windows (Chocolatey — prerelease)

```powershell
choco source add -n=whisper-dictate `
  -s="https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json"
choco install whisper-dictate `
  --version=1.22.0-rc.1 --source=whisper-dictate --prerelease --force -y
```

`--force` lets Chocolatey downgrade/reinstall over a final release you
already have installed. Drop it if this is a fresh box.

### Windows (installer download)

Grab `whisper-dictate-windows-setup-1.22.0-rc.1.exe` from the release
page and run it:

<https://github.com/FactusConsulting/whisper-dictate/releases/tag/v1.22.0-rc.1>

The installer drops the app in
`%LOCALAPPDATA%\Programs\WhisperDictate` and adds `whisper-dictate.exe`
to your user PATH.

### Linux (Ubuntu 26.04 zip)

Homebrew is skipped for prereleases. Grab the Linux zip from the release
page instead:

<https://github.com/FactusConsulting/whisper-dictate/releases/tag/v1.22.0-rc.1>

Unzip it somewhere on your `$PATH` (e.g. `~/.local/bin/`) and confirm:

```bash
whisper-dictate --version   # should print 1.22.0-rc.1
```

## Opting in to the Rust engine

Set the env var before launching, in the SAME shell:

### Windows (PowerShell)

```powershell
$env:VOICEPI_DICTATE_ENGINE = "rust"
& "$env:LOCALAPPDATA\Programs\WhisperDictate\whisper-dictate.exe" run
```

Or, when using the UI shortcut, set the variable at user scope so the
Start-menu launcher inherits it:

```powershell
[Environment]::SetEnvironmentVariable("VOICEPI_DICTATE_ENGINE", "rust", "User")
# then launch Whisper Dictate from the Start menu
```

### Linux (bash)

```bash
VOICEPI_DICTATE_ENGINE=rust whisper-dictate run
```

## What to expect

On startup you should see the dispatch log line on stderr:

```text
[runtime] dictate engine=rust: subprocessing whisper-dictate dictate-run
  (binary='...whisper-dictate.exe', config=None)
[runtime] dictate engine=rust: ready-signal received; forwarding events
  to supervisor
```

From there it should behave identically:

- Same push-to-talk hotkey (right Shift + right Ctrl by default).
- Same status/utterance events surfaced by the UI.
- Same transcript injection into the focused window.

### Automatic fallback

If the Rust engine cannot start (binary missing, features not compiled
in, subprocess spawn error, or an early crash before the READY signal),
the Python worker logs the reason and falls back to the Python engine
for this session. You will see one of:

```text
[runtime] dictate engine=rust: `whisper-dictate` binary not found ...
  falling back to python engine
[runtime] dictate engine=rust: subprocess exited without READY signal
  (code=1); falling back to python engine
```

A broken opt-in must **never** take down the whole worker — that
guarantee is the whole point of Phase A.

## Rollback

Unset the env var. The default is Python, so this is one line:

### Windows (PowerShell)

```powershell
Remove-Item Env:VOICEPI_DICTATE_ENGINE
# Or, if you set it at user scope:
[Environment]::SetEnvironmentVariable("VOICEPI_DICTATE_ENGINE", $null, "User")
```

### Linux (bash)

```bash
unset VOICEPI_DICTATE_ENGINE
```

Restart the worker (or log out/in on Linux if you set it globally) and
you are back on the Python engine.

## Verification (headless smoke)

The Wayland user-smoke script exercises every user-facing feature that
can be tested without a display + audio, including the dispatch selector
that Phase A adds:

```bash
bash scripts/integration/wayland-user-smoke.sh
```

The relevant sections:

- `dictate-run CLI` — confirms `whisper-dictate dictate-run --help`
  parses and the `--json-events` flag is wired.
- `dictate engine dispatch (VOICEPI_DICTATE_ENGINE=rust opt-in)` —
  confirms the Python runtime resolves the env var to the Rust engine.

Neither of those exercises real PTT; they just guard the plumbing so a
future refactor can't silently drop the branch.

## Manual verification: in-process post-processing (Windows-first)

The in-process Rust engine now runs the LLM post-processing pass itself
(the `SessionPostProcess` backend attached in
`runtime::rust_session_real_backends::make_real_session`). Because that
constructor needs a real Whisper model **and** a live capture device, it
cannot be exercised by a headless unit test — CI covers the settings
parser + attach decision in isolation, but the end-to-end attachment must
be verified manually on a real machine before promoting the RC. Do this
on **Windows first** (the primary desktop), then repeat on Ubuntu
Wayland/X11:

1. Configure a post-processor in Settings → Post (or set
   `VOICEPI_POST_PROCESSOR=ollama` and `VOICEPI_POST_MODE=clean`, with a
   local Ollama running). Cloud providers need an API key saved in
   Settings.
2. Launch with the in-process engine:
   `VOICEPI_DICTATE_ENGINE=rust` (PowerShell:
   `$env:VOICEPI_DICTATE_ENGINE = "rust"`).
3. Dictate a short utterance and confirm, in the log/history:
   - the status sequence shows `recording → transcribing →
     post-processing → …`, and
   - the utterance carries the `post_*` fields (`post_processor`,
     `post_mode`, `post_latency_ms`, …) so the UI reads
     **"Post-processing: clean (ollama)"** rather than
     "Post-processing off".
4. Confirm a **provider failure is non-fatal**: stop Ollama (or use a
   bad base URL) and dictate again — the raw transcript must still be
   injected, `post_fallback=true` and `post_error` should be recorded,
   and the loop must not wedge.
5. Confirm `VOICEPI_POST_PROCESSOR=none` (the default) attaches no
   backend: no `post-processing` status, no `post_*` fields.

## Reporting issues

If the Rust engine wedges, hangs, or misbehaves after N attempts:

1. **Capture the log.** Copy the terminal output, especially every line
   starting with `[runtime]`.
2. **Roll back.** `Remove-Item Env:VOICEPI_DICTATE_ENGINE` (PowerShell)
   or `unset VOICEPI_DICTATE_ENGINE` (bash) drops you back to the Python
   engine.
3. **Open an issue** at
   <https://github.com/FactusConsulting/whisper-dictate/issues> with
   the log, the iteration number the wedge appeared on, and your
   platform (Windows / Ubuntu Wayland / Ubuntu X11).

The Phase A rollback story is deliberately zero-blast-radius: worst case
you flip the env var off and continue.

## What's next

Phase B (default flips to Rust with auto-fallback + chaos tests) is
pending user feedback on Phase A. See the
[design doc](./design/item5-wire-dictate-session.md) — sections "Phase
B" and "Phase C" — for the shape of what comes after this prerelease
bakes cleanly.
