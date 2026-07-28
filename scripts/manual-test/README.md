# Manual tests

Hardware-/GUI-dependent checks that CI cannot run (the CI runners have no real
audio device and no interactive desktop). Use these when validating a release
candidate on a real Windows machine.

## `windows-rust-flip-smoke.ps1`

Automated CLI-level smoke for the Rust-default flips of the Python-removal
roadmap (#348): the mic picker (`devices`), post-processing (`postprocess`), and
the external-api chat shell-out. It builds the console CLI with `audio-capture`
and asserts each contract, printing `PASS`/`FAIL` per check. Run it from
anywhere in the repo — it locates the crate root from its own path:

```powershell
.\scripts\manual-test\windows-rust-flip-smoke.ps1
```

Optional — also exercise a real cloud post-process call (otherwise skipped):

```powershell
$env:VOICEPI_POST_API_KEY = "gsk_..."   # Groq (or an OpenAI key)
.\scripts\manual-test\windows-rust-flip-smoke.ps1
```

What it covers:

- **Mic enumeration (Rust `devices` flip, #568/#569)** — the picker request
  (`include_directsound: true`) returns ≥1 mic and no bogus "Primary Sound
  Capture Driver" alias; the plain request (cpal/WASAPI only) still lists mics;
  the `VOICEPI_DEBUG_DIRECTSOUND=1` diagnostic confirms DirectSound actually
  enumerated devices.
- **Post-processing (Rust `postprocess` flip, #566)** — `raw` passthrough returns
  the text unchanged with no network; an unresolvable host yields a
  `fallback_kind = "transport"` envelope (the fall-through-to-Python signal).
- **external-api (#567)** — an empty key classifies `terminal` (no retry / no
  double-charge); an unresolvable host classifies `transport`.

## Windows Credential Manager -> worker key injection

**Not covered by any automated test, on purpose.** The Rust launcher resolves a
saved cloud API key and passes it to the worker in the child's environment
(`runtime::cloud_api_keys`). The shell smoke in
`scripts/integration/wayland-user-smoke.sh` sets `VOICEPI_DISABLE_OS_KEYRING=1`,
so it exercises only the `api-keys.json` file fallback -- never Windows
Credential Manager, which is the PRIMARY store on the primary supported
desktop.

Writing an automated check for a store this machine cannot reach would produce
a test whose passing means nothing. So it is written down as a human step
instead:

1. In Settings -> Speech, paste a cloud STT key and click **Save API key**.
   The status line must say it was stored in the credential store (not only
   the fallback file).
2. Confirm it really is in Credential Manager, not just the file:

   ```powershell
   # Should list a whisper-dictate entry for stt-api-key:<provider>
   cmdkey /list | Select-String whisper-dictate
   # And the file fallback must NOT be the only copy:
   Get-Content "$env:APPDATA\whisper-dictate\api-keys.json" -ErrorAction SilentlyContinue
   ```

3. Close the app. In a NEW PowerShell with no key exported:

   ```powershell
   Remove-Item Env:VOICEPI_STT_API_KEY, Env:OPENAI_API_KEY, Env:GROQ_API_KEY -ErrorAction SilentlyContinue
   whisper-dictate run
   ```

   It must start and reach `api ready`. The failure this guards against is
   `x startup error: openai API requires OPENAI_API_KEY, ...` -- which means
   the launcher never read the credential store.
4. Repeat with a cloud **post-processor** configured (`post_processor=groq` or
   `openai`) and only the post key saved. **`api ready` alone is not enough
   here** (Codex P2 #672 `PRRT_kwDOSfNjQs6UZY9r` cmt 3665545681): startup
   loads the post settings but the credential is only validated when
   `postprocess_text` actually processes an utterance
   (`src/python/whisper_dictate/vp_dictate.py:384-385`). To exercise the
   post-key path you MUST trigger at least one utterance through the
   post-processor and observe one of the following as evidence the saved
   post key reached the worker:

   - The dictation-history entry's `post_processor` block shows a
     non-empty `provider` + non-empty `changed`/`text` (i.e. the cleanup
     actually ran). Setting `metrics_jsonl=<path>` in the config
     surfaces the same payload as JSONL if the UI history is
     inconvenient.
   - OR the runtime-log tab shows a `[post] cleaned in Nms via <provider>`
     line (or the Rust `postprocess::run` equivalent) for that
     utterance.
   - OR, on Windows, a `netsh trace` / Fiddler capture of the
     dictation confirms the outgoing Authorization header carries the
     saved key value (redact before pasting).

   A `post_error` containing `refusing to send stored post-processing
   key` is ALSO a valid pass -- it means #666 landed and the key
   correctly refused an unrelated endpoint. If instead you see
   `post_error` containing `requires OPENAI_API_KEY` / `requires
   GROQ_API_KEY`, the saved post key did NOT reach the worker: same
   FAIL as step 3.

Record the result in the release-candidate notes. If step 3 fails, or
step 4 fails to produce ANY of the evidence lines above (i.e. the
utterance never invoked the post-processor at all), the launcher
credential path is broken on Windows regardless of what the Linux smoke
says.

### Recording template (fill in and paste into the RC release notes)

Per Codex P2 #642 (`PRRT_kwDOSfNjQs6UJFJb`): "written steps alone do not
verify" -- an RC does not ship until the checklist is executed on Windows AND
the outcome is captured verbatim. Copy the template below into the RC notes
and fill in the actual output (or "OK" if the observed output matches the
expected line):

```markdown
### Manual: Windows Credential Manager -> worker key injection (docs/manual-test/README.md)

- Machine / OS:              e.g. ThinkPad X13 / Windows 11 24H2
- Whisper-dictate version:   1.22.0-rc.N
- Date / tester:             YYYY-MM-DD / <initials>
- Step 1 (Save API key status line):     <paste>
- Step 2a (`cmdkey /list`):              <paste one line>
- Step 2b (`api-keys.json` present?):    yes / no
- Step 3 (`whisper-dictate run` output): <paste 3-5 lines ending at `api ready` or the error>
- Step 4a (post-processor utterance):    ran / did-not-run
- Step 4b (post-key evidence line):      <paste history entry / [post] cleaned line / capture line>
- Result:                                PASS / FAIL / BLOCKED (with reason)
```

If step 3 fails, or step 4a is `did-not-run`, or step 4b is empty,
DO NOT tag `v1.22.0`. Open a bug with the pasted output and hand back to
the launcher credential-wiring owner. The unit tests (`credentials::tests`,
`runtime::cloud_api_keys::cloud_api_keys_tests`,
`ui::cloud_settings_tests::ui_worker_command_*`) cover the resolution logic
but cannot reach the real OS keyring on this machine, and startup-only
verification cannot catch a credential that is loaded but never sent.

## Full-app checklist (run the actual GUI / worker)

The script covers the CLI contracts; these need the real app running:

1. **Mic picker** — Settings → mic dropdown lists all your mics with the correct
   default. Select a non-default one, dictate, confirm it records from it.
2. **Post-processing** — enable a cleanup mode (e.g. `clean` with a cloud key),
   dictate a messy sentence, confirm the injected text is cleaned. Repeat with
   mode `raw` and confirm it is passed through untouched.
3. **Opt-outs** — set `VOICEPI_POSTPROCESS_BACKEND=python` and, separately,
   `VOICEPI_DEVICES_BACKEND=python`; restart and repeat 1–2. Behaviour must be
   identical — that exercises the sounddevice / in-process Python fallbacks.
4. **Enterprise / proxy path (if applicable)** — behind a corporate proxy or
   private CA, confirm post-processing still cleans text (the transport-fallback
   net from #566/#567 hands off to Python `urllib`).
5. **No console flashes** — shelling out to the Rust helper from the tray worker
   must not pop a black console window (the two-binary split, #564).
