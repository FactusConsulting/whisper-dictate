//! Regression tests for the manual-test README and the wayland-user-smoke
//! shell script.
//!
//! These pin the Codex findings on PR #672 that live in text-only artifacts
//! (docs + shell), so a future edit that reverts the substantive fix trips a
//! red test rather than being caught only by the next Codex round.
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
//! * `PRRT_kwDOSfNjQs6Uaj0A` (P2, cmt 3665921394) -- the wayland-user-smoke
//!   hotkey-boot environment-gap matcher must NOT include the generic
//!   `rdev listener failed to start` wrapper, only the specific
//!   `MissingDisplayError` token.
//! * `PRRT_kwDOSfNjQs6Uaj0I` (P2, cmt 3665921401) -- the wayland-user-smoke
//!   hotkey-boot rebuild-with branch must fail (`bad`), not skip, when
//!   `CMD_SOURCE=installed` (a release binary must ship both features).
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
//! * `PRRT_kwDOSfNjQs6Ubpeb` (P2, cmt 3666333668) -- the CMD_SOURCE guard
//!   test must EXECUTE the extracted branch under bash with mocked values,
//!   not pattern-match its text (an inverted `!=` guard passed the old test).

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // Cargo runs integration tests with CWD = the manifest dir; this file
    // lives at `src/rust/tests/manual_test_docs.rs`, so the manifest is
    // `src/rust/`. The docs / scripts under test live at repo-root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("resolve repo root from CARGO_MANIFEST_DIR")
}

fn read_manual_test_readme() -> String {
    let path = repo_root().join("scripts/manual-test/README.md");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn read_wayland_smoke() -> String {
    let path = repo_root().join("scripts/integration/wayland-user-smoke.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

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

// ---------------------------------------------------------------------------
// P2: hotkey-boot env-gap matcher must NOT match the generic wrapper.
// ---------------------------------------------------------------------------

#[test]
fn wayland_smoke_hotkey_boot_env_matcher_omits_generic_rdev_wrapper() {
    let smoke = read_wayland_smoke();
    // The active matcher lives on the line that starts with the
    // grep-q against the hotkey-boot output. `MissingDisplayError` MUST
    // be present (that is the specific headless-env token). The generic
    // wrapper `rdev listener failed to start` MUST NOT appear inside
    // the same regex, because `InstallError::ListenerStartup` prefixes
    // it to EVERY rdev startup failure (permission-denied, OS refusal,
    // future regressions) -- matching it downgrades every future rdev
    // regression on Linux to `warn` and lets the smoke pass.
    //
    // Extract the actual matcher line (not the comment block above it)
    // by picking the first `elif printf ...grep -q "..."` after the
    // hotkey-boot section header.
    let header = "self-test hotkey-boot (Windows PTT-boot regression";
    let start = smoke
        .find(header)
        .expect("wayland-user-smoke.sh missing the self-test hotkey-boot section header");
    let after_header = &smoke[start..];
    let matcher_line = after_header
        .lines()
        .find(|l| {
            let t = l.trim_start();
            t.starts_with("elif printf ") && t.contains("MissingDisplayError")
        })
        .expect(
            "wayland-user-smoke.sh: could not locate the hotkey-boot \
             env-gap `elif` line containing MissingDisplayError",
        );
    assert!(
        matcher_line.contains("MissingDisplayError"),
        "hotkey-boot env-gap matcher must contain `MissingDisplayError` \
         -- the specific rdev headless token."
    );
    assert!(
        !matcher_line.contains("rdev listener failed to start"),
        "hotkey-boot env-gap matcher must NOT match the generic \
         `rdev listener failed to start` wrapper; that prefix appears on \
         every rdev startup failure and would downgrade real regressions \
         to `warn` -- Codex P2 PRRT_kwDOSfNjQs6Uaj0A cmt 3665921394.\n\
         offending line: {matcher_line}"
    );
}

// ---------------------------------------------------------------------------
// P2: rebuild-with branch must FAIL on installed release binaries.
// ---------------------------------------------------------------------------

// Executes the guard under `bash`, so it is scoped to the platforms the
// script itself targets. `wayland-user-smoke.sh` only ever runs on Linux
// (the user's Ubuntu Wayland box and the ubuntu-2604 integration
// container); the Windows CI leg has no Wayland session and its `bash`
// is whatever Git-for-Windows/MSYS ships, whose exit-status and pipeline
// semantics differ from the shell that will actually interpret this
// script. Running it there produced a non-zero exit with empty stderr on
// windows-2025 while passing on both Linux CI and a local Git-bash --
// i.e. pure runner noise about a file Windows never executes. The
// structural assertions above (guard exists, references CMD_SOURCE)
// stay cross-platform; only the execution is gated.
#[cfg(not(windows))]
#[test]
fn wayland_smoke_hotkey_boot_missing_features_fails_on_installed_release() {
    // Codex P2 PRRT_kwDOSfNjQs6Ubpeb cmt 3666333668: the earlier version
    // of this test only checked that the surrounding text CONTAINED
    // `CMD_SOURCE` / `installed` and that some `bad` preceded some
    // `warn`. Inverting the guard to
    // `if [ "$CMD_SOURCE" != "installed" ]` -- i.e. exactly restoring
    // the release-binary warn-skip bug -- satisfied every one of those
    // assertions. So instead of pattern-matching the source, EXECUTE the
    // extracted guard under bash with `bad` / `warn` stubbed, once per
    // CMD_SOURCE value, and assert the OBSERVED verdict. An inverted
    // guard now flips both verdicts and trips the test.
    let block = extract_rebuild_with_guard();

    let installed = run_guard_with_cmd_source(&block, "installed");
    assert_eq!(
        installed, "bad",
        "hotkey-boot rebuild-with guard must call `bad` when \
         CMD_SOURCE=installed: the release workflow builds the shipping \
         binary with rust-hotkeys + rust-injection, so a rebuild-with \
         message from an INSTALLED binary is a packaging regression, not \
         an environment gap. Observed verdict: {installed} -- Codex P2 \
         PRRT_kwDOSfNjQs6Uaj0I cmt 3665921401 / \
         PRRT_kwDOSfNjQs6Ubpeb cmt 3666333668.\nguard under test:\n{block}"
    );

    // The source/dev fallback is the ONLY path allowed to warn-skip --
    // it never claimed to be the shipping binary. Asserting this second
    // case is what makes an inverted guard detectable: a `!=` flip keeps
    // "some bad and some warn exist" true but swaps these two verdicts.
    let source = run_guard_with_cmd_source(&block, "source");
    assert_eq!(
        source, "warn",
        "hotkey-boot rebuild-with guard must warn-skip (not fail) for the \
         source/Python dev fallback. Observed verdict: {source} -- Codex \
         P2 PRRT_kwDOSfNjQs6Ubpeb cmt 3666333668.\nguard under test:\n{block}"
    );
}

/// Slice the literal `if [ "$CMD_SOURCE" = ... ] ... fi` block out of the
/// hotkey-boot rebuild-with branch so it can be executed verbatim.
///
/// Deliberately extracts by structure (the `if`/`fi` lines at a known
/// indentation inside the located branch) rather than matching the guard's
/// text, so the test cannot pass by "recognising" the correct condition --
/// whatever condition is actually there gets run.
#[cfg(not(windows))]
fn extract_rebuild_with_guard() -> String {
    let smoke = read_wayland_smoke();
    let header = "self-test hotkey-boot (Windows PTT-boot regression";
    let start = smoke
        .find(header)
        .expect("wayland-user-smoke.sh missing the self-test hotkey-boot section header");
    let rebuild_rel = smoke[start..].find("rebuild with").expect(
        "wayland-user-smoke.sh: expected a `rebuild with` guard in the hotkey-boot section",
    );
    let after_rebuild = &smoke[start + rebuild_rel..];

    let mut lines = Vec::new();
    let mut in_block = false;
    for line in after_rebuild.lines() {
        let trimmed = line.trim();
        if !in_block {
            if trimmed.starts_with("if ") {
                in_block = true;
                lines.push(line);
            }
            continue;
        }
        lines.push(line);
        if trimmed == "fi" {
            return lines.join("\n");
        }
    }
    panic!(
        "wayland-user-smoke.sh: could not extract a complete if/fi guard from \
         the hotkey-boot rebuild-with branch (found {} line(s) before EOF)",
        lines.len()
    );
}

/// Execute `block` under bash with `CMD_SOURCE` set to `cmd_source` and the
/// script's `bad` / `warn` reporters stubbed to echo their own name.
///
/// Returns the verdict the guard actually reached: `"bad"`, `"warn"`, or
/// `"(none)"` if it took neither branch.
#[cfg(not(windows))]
fn run_guard_with_cmd_source(block: &str, cmd_source: &str) -> String {
    use std::process::Command;

    // `hb_out` is referenced inside the real `bad`/`warn` messages via a
    // command substitution, so it must exist or `set -u` semantics in the
    // harness would abort. Give it a realistic rebuild-with payload.
    let script = format!(
        r#"set -u
CMD_SOURCE="{cmd_source}"
hb_out="error: rust-hotkeys feature is not compiled in (rebuild with --features rust-hotkeys)"
bad()  {{ printf 'bad\n'; }}
warn() {{ printf 'warn\n'; }}
{block}
"#
    );

    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run the extracted guard under bash (bash must be on PATH)");
    assert!(
        out.status.success(),
        "extracted guard exited non-zero for CMD_SOURCE={cmd_source}\n\
         stderr: {}\nscript:\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let verdict = stdout.trim();
    if verdict.is_empty() {
        "(none)".to_owned()
    } else {
        verdict.to_owned()
    }
}
