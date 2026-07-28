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
   `openai`) and only the post key saved, to cover the second account name.

Record the result in the release-candidate notes. If step 3 fails, the launcher
credential path is broken on Windows regardless of what the Linux smoke says.

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
- Step 4 (post-key path variant):        pass / fail  (paste output if fail)
- Result:                                PASS / FAIL / BLOCKED (with reason)
```

If step 3 or 4 fails, DO NOT tag `v1.22.0`. Open a bug with the pasted output
and hand back to the launcher credential-wiring owner. The unit tests
(`credentials::tests`, `runtime::cloud_api_keys::cloud_api_keys_tests`,
`ui::cloud_settings_tests::ui_worker_command_*`) cover the resolution logic
but cannot reach the real OS keyring on this machine.

## Windows post-processing endpoint-marker verification (Codex #642 / #666 P1 chain)

**Not covered by any automated test on Windows**, though the underlying wiring
is unit-tested in `postprocess::endpoint_marker_tests`,
`runtime::cloud_api_keys::cloud_api_keys_tests`, and
`ui::cloud_settings_tests::ui_worker_command_*` (all run on `windows-2025`
in CI). Codex P1 #666 (`PRRT_kwDOSfNjQs6UYvxh`, cmt 3665244987) asks for a
release-time end-to-end pass through the installed tray app because the
`WorkerCommand` unit tests do not exercise the actual Windows process
environment, credential store, or in-process controller path.

Run this once per release candidate against the SIGNED installer built by
`.github/workflows/release.yml` (not a local `build-installer.ps1` output --
that ships unsigned per memory `windows-installer-signing.md`):

1. Install the RC (`Output/whisper-dictate-*-Setup.exe`) into a fresh user
   profile so the credential store starts empty.
2. Settings -> Speech -> paste a **Groq** STT key, save. Settings ->
   Post-processing -> select **Groq**, mode **clean**. Do NOT paste a
   post-processing-specific key; the UI mirror should reuse the Groq STT key.
3. Click Start. Confirm the tray goes green and Health -> Post-processing
   reports the Groq endpoint.
4. **Cross-provider leak check**: In Settings, change Post-processing
   processor to **OpenAI** (keep the URL empty -> the default OpenAI endpoint
   substitutes). Save. Trigger a dictation.

   Expected: the metrics envelope shows a `post_error` containing
   `refusing to send stored post-processing key to a different endpoint`
   (or `refusing to send stored post-processing key over plaintext http://`
   if the URL was scheme-downgraded). The tray must NOT show a successful
   post-processing pass.

   To confirm no request left the machine, run a local capture beforehand
   (e.g. `netsh trace start` or Fiddler) and verify no connection to
   `api.openai.com` was made during the dictation.
5. **Custom-origin leak check**: Same as (4) but paste
   `https://llm.internal.example/v1` as the post base URL. Same expected
   outcome (`refusing to send ... to a different self-hosted origin`).
6. **Legitimate live change**: Restart the application (per the message).
   Confirm post-processing works for the new endpoint if you now paste an
   OpenAI post key (or configure the correct provider). This validates the
   recovery advice in the error message.

Record each step's outcome in the RC template above (append a new block titled
`Endpoint-marker leak check`). If any leak-check step results in a real
provider request being made with the stored key, the fix regressed and the
RC must not ship.

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
