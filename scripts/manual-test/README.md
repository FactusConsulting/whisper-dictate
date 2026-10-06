# Manual tests

Hardware-/GUI-dependent checks that CI cannot run (the CI runners have no real
audio device and no interactive desktop). Use these when validating a release
candidate on a real Windows machine.

## `windows-rust-flip-smoke.ps1`

Automated CLI-level smoke for the native mic picker (`devices`), post-processing
(`postprocess`), and external-api chat shell-out. It builds the console CLI with `audio-capture`
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
- **Post-processing (native Rust path, #566)** — `raw` passthrough returns
  the text unchanged with no network; an unresolvable host yields a
  `fallback_kind = "transport"` envelope.
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

3. Close the app. In a NEW PowerShell with NO key exported
   (`VOICEPI_POST_API_KEY` MUST be removed too -- both
   `runtime::cloud_api_keys` and the native credential resolver prefer
   the environment value, so an ambient post key inherited from the
   "optional cloud-call setup" at the top of this file would mask a
   broken Credential Manager lookup in step 4):

   ```powershell
   Remove-Item Env:VOICEPI_STT_API_KEY, Env:VOICEPI_POST_API_KEY, `
               Env:OPENAI_API_KEY, Env:GROQ_API_KEY `
               -ErrorAction SilentlyContinue
   wd run
   ```

   It must start and reach `api ready`. The failure this guards against is
   `x startup error: openai API requires OPENAI_API_KEY, ...` -- which means
   the launcher never read the credential store.
4. Now exercise the cloud **post-processor** with only its post-specific
   credential in the store. **First delete the saved STT credential for
   the same provider**: `credentials::resolve_post_api_key` deliberately
   falls back to the STT account for the same provider, so leaving the
   step-1 STT credential in place means the required successful
   utterance could pass on the STT fallback even when the post-specific
   `post-api-key:<provider>` lookup is BROKEN -- masking the exact
   regression this step exists to catch.

   ```powershell
   # Delete the saved STT credential first so the post-key path is
   # exercised without any cross-account fallback. The credential
   # target name Windows Credential Manager sees is `<user>.<service>`
   # per `credential_target_name` in `src/rust/ui/api_keys.rs` --
   # `whisper-dictate/stt-api-key:<provider>` does NOT match the target
   # the app writes, so that form silently no-ops the delete and step
   # 4's post-key regression stays masked by the STT fallback path.
   # Verify the entry is actually gone before continuing.
   # ONE variable drives the delete AND the verification, so the two
   # cannot disagree: hard-coded `groq` examples are wrong for a tester
   # who deletes OpenAI instead -- they would let the DELETED OpenAI
   # credential survive, and `resolve_post_api_key`'s same-provider STT
   # fallback would then mask a broken `post-api-key:openai` readback.
   # Set it to the provider whose post key step 4b will exercise.
   $deleted = "groq"                                 # or "openai"
   cmdkey /delete:"stt-api-key:${deleted}.whisper-dictate"
   # Literal form for reference: cmdkey /delete:stt-api-key:groq.whisper-dictate
   # Verify ONLY the DELETED provider's entry is gone. Always qualify the
   # filter with `$deleted` -- never match a bare `stt-api-key`, and never
   # hard-code one provider: the alternate-provider escape hatch below
   # deliberately KEEPS the OTHER provider's STT credential, so a blanket
   # "must return NOTHING" gate would fail a valid setup and make this
   # release gate impossible to pass.
   cmdkey /list | Select-String "stt-api-key:${deleted}"  # must return NOTHING
   cmdkey /list | Select-String "stt-api-key:"            # only NON-$deleted entries may remain
   # Then either save the post key via Settings -> Post-processing ->
   # Save API key (which writes `post-api-key:<provider>`), or set
   # `post_processor=groq`/`openai` in config and save through the UI.
   ```

   **Switch STT off the now-keyless cloud provider before dictating**.
   Deleting the STT credential above makes `cloud_stt_missing_api_key()`
   true whenever `stt_backend == "openai"` and the provider is not
   `Custom`, and `start_runtime` returns BEFORE launching the worker in
   that case. The tester would then be unable to produce the step-4
   utterance at all, so the release gate could never be completed as
   written. Do ONE of:

   - **Preferred** — Settings -> Speech -> set the STT backend to
     **local Whisper** (`stt_backend` = `whisper` -- `local` is NOT a
     valid value: `AppSettings::validate` accepts only `whisper` and
     `openai` via `validate_choice("stt_backend", ...)`, and the UI's
     "Local Whisper" option stores `whisper`, so a tester who writes
     `local` gets a config that fails validation instead of the working
     backend the step-4 utterance needs). Local STT needs no
     credential, so `cloud_stt_missing_api_key()` is false and the
     worker starts; the post-processor still exercises the
     `post-api-key:<provider>` lookup, which is the ONLY thing this
     step is measuring.
   - **Or** — keep cloud STT but point it at a DIFFERENT provider whose
     STT credential is still saved (e.g. delete `stt-api-key:groq` and
     leave `stt-api-key:openai` in place while post-processing uses
     Groq). `resolve_post_api_key`'s STT fallback is per-provider, so
     a different-provider STT key cannot mask the post-key lookup
     under test.

   Do NOT simply re-save the STT key you just deleted -- that restores
   the cross-account fallback and the step measures nothing.

   **Close the saving app and re-launch with a scrubbed environment
   before step 4b**: Settings -> Save keeps the plaintext post key in
   `post_api_key_input`, and `worker_command` injects that in-memory
   value directly. Dictating in the same process
   would therefore succeed even if reading `post-api-key:<provider>`
   back from Windows Credential Manager is completely broken, so
   the post-credential regression this step exists to catch stays
   masked. Do exactly what step 3 already does for STT: exit the
   app, open a NEW PowerShell with the key environment scrubbed,
   then launch `whisper-dictate` fresh for the utterance below:

   ```powershell
   # Exit the app first (close the window / Ctrl+C the CLI), then:
   Remove-Item Env:VOICEPI_STT_API_KEY, Env:VOICEPI_POST_API_KEY, `
               Env:OPENAI_API_KEY, Env:GROQ_API_KEY `
               -ErrorAction SilentlyContinue
   wd run   # fresh process, no in-memory post_api_key_input
   ```

   With the STT credential gone AND the app relaunched fresh,
   configure a cloud post-processor. **`api ready` alone is not
   enough here**: startup loads the post settings but the credential
   is only validated when `postprocess_text` actually processes an
   utterance (the native Rust dictation path). To exercise
   the post-key path you MUST trigger at least one utterance through
   the post-processor and observe one of the following as evidence the
   saved post key reached the worker AND the provider request
   succeeded:

   - The dictation-history JSONL entry has a non-empty
     **`post_processor`** AND **`post_fallback == false`** AND an
     EMPTY **`post_error`**. These are FLAT top-level keys, not a
     nested block: `_history_event` in
     the native history sink writes exactly
     `post_processor`, `post_mode`, `post_model`, `post_latency_ms`,
     `post_changed`, `post_fallback`, `post_error` at the top level,
     and the UI history preview renders that JSONL directly -- there
     is no `post_processor.provider` field to look for.
     Why all three: a failed provider request returns the original
     text and KEEPS the configured processor name in the envelope
     while setting `post_fallback=true` + a `post_error`, so a
     non-empty `post_processor` on its own is NOT enough.
     `post_changed` MAY
     be false because a successful cleanup can legitimately return
     unchanged text.
     If the UI history is inconvenient, the same payload can be
     written as JSONL -- but you must set **BOTH** `inject_json=true`
     AND `metrics_jsonl=<path>`: `append_record_sinks` (the native
     record-sink path) only honours `metrics_jsonl` when
     `json_output` is truthy, and `inject_json` defaults to `false`
     on the fresh profile step 1 requires, so setting the path
     alone leaves the promised file absent.
   - The native runtime records successful post-processing in the
     utterance-card fields (`provider`, `fallback=false`, `error` empty).
     Use those structured fields as the acceptance signal; the UI may
     suppress raw diagnostic lines while the card is visible.
   - OR, on Windows, a `netsh trace` / Fiddler capture of the
     dictation confirms the outgoing Authorization header carries the
     saved key value AND the server responded with a 2xx (redact
     credentials before pasting).

   A `post_error` containing `refusing to send stored post-processing
   key` is a **FAIL**, not a pass -- it is not a passing outcome
   "because the key reached the worker". Two reasons:

   - The endpoint-marker guard (`require_endpoint_matches_marker`)
     refuses
     BEFORE any HTTP request and returns a `terminal` fallback envelope
     (`post_fallback=true` + that `post_error`). So the provider
     round-trip this step measures never happened -- the run produces
     none of the evidence above and proves nothing about
     `post-api-key:<provider>` being readable.
   - In the different-provider escape hatch it is in fact the SIGNATURE
     of the exact regression step 4 exists to catch. If reading
     `post-api-key:<provider>` back from Credential Manager is broken,
     `load_post_api_key_state` leaves `post_api_key_input` empty on
     the fresh process, `worker_command` then mirrors the still-saved
     OTHER provider's STT key with `SttMirror` provenance, and
     `stamp_post_api_key_endpoint_marker` binds the marker to that
     STT endpoint. The
     guard then refuses precisely because the WRONG key reached the
     worker. Treating that as a pass would ship the broken readback.

   The only benign cause is a live `post_processor` / `post_base_url`
   change made after the worker started; in that case fix the endpoint,
   relaunch per the scrubbed-environment step above and re-run 4a/4b --
   the refusing run is not evidence either way. A refusal from a fresh
   process whose post endpoint matches the saved key is a hard FAIL.

   Every other `post_error` (`requires OPENAI_API_KEY`,
   `requires GROQ_API_KEY`, HTTP 401/403/429, `transport`, `terminal`)
   is a FAIL for the same reason: either the saved post key did NOT
   reach the worker (Credential Manager regression, same class as
   step 3) OR the key reached but the provider rejected it (bad key,
   expired, revoked). Either way DO NOT tag the RC.

Record the result in the release-candidate notes. If step 3 fails, or
step 4 fails to produce ANY of the evidence lines above (i.e. the
utterance never invoked the post-processor at all), the launcher
credential path is broken on Windows regardless of what the Linux smoke
says.

### Recording template (fill in and paste into the RC release notes)

Written steps alone do not verify: an RC does not ship until the checklist
is executed on Windows AND the outcome is captured verbatim. Copy the
template below into the RC notes
and fill in the actual output (or "OK" if the observed output matches the
expected line):

```markdown
### Manual: Windows Credential Manager -> native runtime key injection

- Machine / OS:              e.g. ThinkPad X13 / Windows 11 24H2
- Whisper-dictate version:   <current-version>
- Date / tester:             YYYY-MM-DD / <initials>
- Step 1 (Save API key status line):     <paste>
- Step 2a (`cmdkey /list`):              <paste one line>
- Step 2b (`api-keys.json` present?):    yes / no
- Step 3 (`wd run` output): <paste 3-5 lines ending at `api ready` or the error>
- Step 4-pre (deleted provider's STT cred gone --
  `cmdkey /list | Select-String "stt-api-key:<deleted-provider>"` empty;
  another provider's STT entry MAY remain, that is the escape hatch): yes / no
- Step 4-pre (STT switched to local Whisper or a different keyed provider): local / other-provider
- Step 4-pre (app restarted fresh with env scrubbed after Settings -> Save): yes / no
- Step 4a (post-processor utterance):    ran / did-not-run
- Step 4b (post-key evidence, paste ONE of):
  - history JSONL entry showing flat `post_processor=<name>`,
    `post_fallback=false`, `post_error=""`:            <paste>
  - Rust utterance card fields (`provider`, `fallback=false`,
    `error` empty):                                    <paste>
  - `netsh trace` / Fiddler 2xx line (credentials redacted): <paste>
  - (a `post_error` with `refusing to send stored post-processing key`
    is NOT one of these alternatives -- it is a FAIL, see above)
- Result:                                PASS / FAIL / BLOCKED (with reason)
```

If step 3 fails, or any of the step 4-pre lines is `no`, or step 4a
is `did-not-run`, or step 4b is empty,
DO NOT tag the release. Open a bug with the pasted output and hand back to
the launcher credential-wiring owner. The unit tests (`credentials::tests`,
`runtime::cloud_api_keys::cloud_api_keys_tests`,
`ui::cloud_settings_tests::ui_worker_command_*`) cover the resolution logic
but cannot reach the real OS keyring on this machine, and startup-only
verification cannot catch a credential that is loaded but never sent.

## Full-app checklist (run the actual GUI / runtime)

The script covers the CLI contracts; these need the real app running:

1. **Mic picker** — Settings → mic dropdown lists all your mics with the correct
   default. Select a non-default one, dictate, confirm it records from it.
2. **Post-processing** — enable a cleanup mode (e.g. `clean` with a cloud key),
   dictate a messy sentence, confirm the injected text is cleaned. Repeat with
   mode `raw` and confirm it is passed through untouched.
3. **Native-only path** — repeat the post-processing and device checks after
   restarting the app with a clean environment. All paths are native;
   failures should be investigated from the Rust diagnostic log.
4. **Enterprise / proxy path (if applicable)** — behind a corporate proxy or
   private CA, confirm post-processing still cleans text. Transport failures
   remain explicit Rust fallback envelopes.
5. **No console flashes** — shelling out to the Rust helper from the tray worker
   must not pop a black console window (the two-binary split, #564).
