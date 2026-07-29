//! Regression tests for the manual-test README.
//!
//! These pin the Codex findings on PR #672 that live in a text-only artifact
//! (`scripts/manual-test/README.md`), so a future edit that reverts the
//! substantive fix trips a red test rather than being caught only by the next
//! Codex round. The findings about `scripts/integration/wayland-user-smoke.sh`
//! live in the sibling `wayland_smoke_guard.rs`.
//!
//! Round 5:
//!
//! * `PRRT_kwDOSfNjQs6Uajz7` (P1, cmt 3665921389) -- README's cmdkey
//!   `/delete:` argument must match the Windows credential-target format
//!   `<user>.<service>` that `credential_target_name` in
//!   `src/rust/ui/api_keys.rs` writes on Windows. The pre-fix value
//!   `whisper-dictate/stt-api-key:groq` silently no-ops.
//! * `PRRT_kwDOSfNjQs6Uaj0Q` (P1, cmt 3665921411) -- README step 4 must
//!   explicitly require closing the saving app and relaunching with a
//!   scrubbed environment, otherwise the in-memory `post_api_key_input`
//!   plaintext masks a broken keyring readback.
//!
//! Round 6:
//!
//! * `PRRT_kwDOSfNjQs6UbpeI` (P1, cmt 3666333641) -- deleting the STT
//!   credential in step 4 strands a cloud STT backend
//!   (`cloud_stt_missing_api_key` blocks `start_runtime`), so the README must
//!   offer local Whisper or a different keyed provider.
//! * `PRRT_kwDOSfNjQs6UbpeP` (P2, cmt 3666333651) -- the history evidence
//!   fields are FLAT top-level JSONL keys (`post_processor`,
//!   `post_fallback`, `post_error`), not a nested block with `provider`.
//! * `PRRT_kwDOSfNjQs6UbpeV` (P2, cmt 3666333658) -- the recording template
//!   must name the real emitted `[post]` signatures, not `[post] cleaned`.
//! * `PRRT_kwDOSfNjQs6UbpeY` (P2, cmt 3666333662) -- offering `metrics_jsonl`
//!   as evidence requires `inject_json=true` in the same breath.

mod common;

use common::read_manual_test_readme;

// ---------------------------------------------------------------------------
// P1: cmdkey /delete target must match credential_target_name on Windows.
// ---------------------------------------------------------------------------

#[test]
fn manual_test_readme_uses_windows_credential_target_format_in_cmdkey_delete() {
    // The Windows credential-target format from `credential_target_name`
    // in `src/rust/ui/api_keys.rs:404-410` is `<user>.<service>` where
    // `<service>` is `whisper-dictate` and `<user>` is e.g.
    // `stt-api-key:groq`. Any `cmdkey /delete:` example in the README
    // MUST use that exact ordering. The Codex-flagged pre-fix form
    // `cmdkey /delete:whisper-dictate/stt-api-key:groq` reverses it and
    // silently no-ops on real Windows.
    let readme = read_manual_test_readme();
    assert!(
        readme.contains("cmdkey /delete:stt-api-key:groq.whisper-dictate"),
        "manual-test README missing the correctly-ordered cmdkey /delete \
         example for the Groq STT credential. The credential target the \
         app writes on Windows is `stt-api-key:groq.whisper-dictate` \
         (`credential_target_name` in `src/rust/ui/api_keys.rs`), so a \
         command that uses `whisper-dictate/stt-api-key:groq` silently \
         no-ops -- Codex P1 PRRT_kwDOSfNjQs6Uajz7 cmt 3665921389."
    );
    assert!(
        !readme.contains("cmdkey /delete:whisper-dictate/stt-api-key"),
        "manual-test README still contains the pre-fix mis-ordered \
         cmdkey /delete example `whisper-dictate/stt-api-key:...` -- \
         Codex P1 PRRT_kwDOSfNjQs6Uajz7 cmt 3665921389."
    );
}

// ---------------------------------------------------------------------------
// P1: Step 4 must require closing the app + fresh env before the post-key
// utterance, otherwise the in-memory plaintext masks a broken keyring.
// ---------------------------------------------------------------------------

#[test]
fn manual_test_readme_requires_app_restart_before_post_key_utterance() {
    let readme = read_manual_test_readme();
    // The fix must explicitly cite Codex thread PRRT_kwDOSfNjQs6Uaj0Q
    // AND direct the tester to relaunch the app with the env scrubbed
    // between "Settings -> Save" and "trigger utterance". Both signals
    // must be present so a partial rewrite that keeps only the cite
    // (or only the instruction) is caught.
    assert!(
        readme.contains("PRRT_kwDOSfNjQs6Uaj0Q"),
        "manual-test README missing the Codex P1 thread cite for the \
         app-restart requirement before step 4b."
    );
    let lower = readme.to_lowercase();
    let mentions_restart = lower.contains("close the saving app")
        || lower.contains("close the saving process")
        || lower.contains("close the app")
        || lower.contains("exit the app")
        || lower.contains("re-launch")
        || lower.contains("relaunch")
        || lower.contains("relaunching")
        || lower.contains("launch") && lower.contains("fresh");
    assert!(
        mentions_restart,
        "manual-test README does not tell the tester to exit / relaunch \
         the app after Settings -> Save; without a fresh process the \
         in-memory `post_api_key_input` masks a broken Credential \
         Manager readback -- Codex P1 PRRT_kwDOSfNjQs6Uaj0Q cmt 3665921411."
    );
    // Recording-template must gate on the app-restart step too, or a
    // tester who paste-fills the template can pass without doing it.
    assert!(
        readme.contains("app restarted fresh"),
        "recording template must include a checkbox that gates on the \
         app being restarted with the env scrubbed after Settings -> \
         Save -- Codex P1 PRRT_kwDOSfNjQs6Uaj0Q cmt 3665921411."
    );
}

// ---------------------------------------------------------------------------
// P1 (round 6): deleting the STT credential must not strand the tester on a
// cloud STT backend that then refuses to start.
// ---------------------------------------------------------------------------

#[test]
fn manual_test_readme_keeps_a_usable_stt_backend_after_deleting_the_credential() {
    // `cloud_stt_missing_api_key()` (`src/rust/ui/app.rs:462-468`) is true
    // whenever `stt_backend == "openai"`, the provider is not `Custom`, and
    // the key input is empty -- and `start_runtime` returns BEFORE launching
    // the worker in that case (`src/rust/ui/app.rs:261-265`). Step 4 deletes
    // exactly that credential, so the README must tell the tester how to keep
    // a startable STT backend or the release gate is uncompletable.
    let readme = read_manual_test_readme();
    assert!(
        readme.contains("PRRT_kwDOSfNjQs6UbpeI"),
        "manual-test README missing the Codex P1 thread cite for keeping a \
         usable STT backend after the step-4 credential deletion."
    );
    let lower = readme.to_lowercase();
    assert!(
        lower.contains("local whisper"),
        "manual-test README must offer switching STT to local Whisper after \
         deleting the cloud STT credential -- otherwise \
         `cloud_stt_missing_api_key()` blocks `start_runtime` and the tester \
         cannot produce the step-4 utterance at all (Codex P1 \
         PRRT_kwDOSfNjQs6UbpeI cmt 3666333641)."
    );
    assert!(
        readme.contains("cloud_stt_missing_api_key"),
        "manual-test README must name `cloud_stt_missing_api_key` so the \
         tester understands WHY the deletion strands a cloud STT backend \
         -- Codex P1 PRRT_kwDOSfNjQs6UbpeI cmt 3666333641."
    );
    // The recording template must capture which escape hatch was used, so a
    // completed template proves the tester actually had a startable backend.
    assert!(
        readme.contains("local / other-provider"),
        "recording template must record which STT escape hatch the tester \
         used (local Whisper vs a different keyed provider) -- Codex P1 \
         PRRT_kwDOSfNjQs6UbpeI cmt 3666333641."
    );
}

// ---------------------------------------------------------------------------
// P2 (round 6): history evidence must name the FLAT JSONL keys, and the
// metrics-file offer must require inject_json too.
// ---------------------------------------------------------------------------

#[test]
fn manual_test_readme_uses_flat_history_field_names_for_post_evidence() {
    // `_history_event` (`src/python/whisper_dictate/vp_history.py:92-105`)
    // emits `post_processor` / `post_fallback` / `post_error` as FLAT
    // top-level keys. A doc that describes a nested `post_processor` block
    // with a `provider` field sends the tester looking for something the
    // JSONL never contains.
    let readme = read_manual_test_readme();
    assert!(
        readme.contains("PRRT_kwDOSfNjQs6UbpeP"),
        "manual-test README missing the Codex P2 thread cite for the flat \
         history field names."
    );
    for field in ["post_processor", "post_fallback", "post_error"] {
        assert!(
            readme.contains(field),
            "manual-test README must name the flat history field `{field}` \
             in the step-4 evidence list -- Codex P2 PRRT_kwDOSfNjQs6UbpeP \
             cmt 3666333651."
        );
    }
    assert!(
        !readme.contains("`post_processor` block"),
        "manual-test README still describes a nested ``post_processor` \
         block``; the JSONL keys are flat top-level fields -- Codex P2 \
         PRRT_kwDOSfNjQs6UbpeP cmt 3666333651."
    );
    assert!(
        readme.contains("FLAT top-level keys"),
        "manual-test README must state explicitly that the post-processing \
         history fields are flat top-level keys, not a nested block -- \
         Codex P2 PRRT_kwDOSfNjQs6UbpeP cmt 3666333651."
    );
}

#[test]
fn manual_test_readme_requires_inject_json_alongside_metrics_jsonl() {
    // `append_record_sinks` (`vp_history.py:47-59`) only honours
    // `metrics_jsonl` when `json_output` is truthy, and `inject_json`
    // defaults to false on the fresh profile step 1 mandates
    // (`src/rust/config/settings.rs:124-125`). Offering the metrics path
    // alone promises a file that never appears.
    let readme = read_manual_test_readme();
    assert!(
        readme.contains("PRRT_kwDOSfNjQs6UbpeY"),
        "manual-test README missing the Codex P2 thread cite for the \
         inject_json requirement."
    );
    let metrics_idx = readme
        .find("metrics_jsonl")
        .expect("manual-test README should still mention metrics_jsonl as an evidence surface");
    // `inject_json` must be required in the same paragraph as the
    // metrics_jsonl offer, not merely mentioned somewhere else in the file.
    let window_start = metrics_idx.saturating_sub(400);
    let window_end = (metrics_idx + 400).min(readme.len());
    let window = &readme[window_start..window_end];
    assert!(
        window.contains("inject_json"),
        "the metrics_jsonl evidence offer must require `inject_json=true` \
         in the same breath -- `append_record_sinks` writes the metrics \
         path only when JSON output is enabled, and `inject_json` defaults \
         to false on a fresh profile (Codex P2 PRRT_kwDOSfNjQs6UbpeY cmt \
         3666333662).\nwindow around metrics_jsonl:\n{window}"
    );
}

// ---------------------------------------------------------------------------
// P2 (round 6): the recording template must name the real emitted [post]
// signatures, not an invented "[post] cleaned" line.
// ---------------------------------------------------------------------------

#[test]
fn manual_test_readme_template_names_the_real_post_success_signatures() {
    // `vp_dictate.py:390-395` emits `[post] <mode>/<provider> <N>ms text=...`
    // or `... unchanged`. The template previously asked for a
    // "`[post] cleaned` line", which the worker never emits, so an operator
    // could not paste the requested evidence verbatim even after a clean run.
    let readme = read_manual_test_readme();
    let template_start = readme
        .find("### Manual: Windows Credential Manager")
        .expect("manual-test README missing the recording template block");
    let template = &readme[template_start..];
    assert!(
        !template.contains("[post] cleaned"),
        "recording template still asks for a `[post] cleaned` line, which \
         the worker never emits -- Codex P2 PRRT_kwDOSfNjQs6UbpeV cmt \
         3666333658."
    );
    assert!(
        template.contains("<N>ms text=") && template.contains("<N>ms unchanged"),
        "recording template must name BOTH real success signatures \
         (`[post] <mode>/<provider> <N>ms text=...` and \
         `... <N>ms unchanged`) so the operator can paste the evidence \
         verbatim -- Codex P2 PRRT_kwDOSfNjQs6UbpeV cmt 3666333658."
    );
    assert!(
        template.contains("post_fallback=false"),
        "recording template must name the flat `post_fallback=false` field \
         for the history-JSONL evidence path -- Codex P2 \
         PRRT_kwDOSfNjQs6UbpeP cmt 3666333651."
    );
}
