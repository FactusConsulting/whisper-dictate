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
//!
//! Round 7:
//!
//! * `PRRT_kwDOSfNjQs6UcarH` (P2, cmt 3666625739) -- every `stt_backend`
//!   value the README hands a tester must be one `AppSettings::validate`
//!   actually accepts. The pre-fix `local` is rejected by
//!   `validate_choice("stt_backend", ..., &["whisper", "openai"])`.
//! * `PRRT_kwDOSfNjQs6UcarQ` (P2, cmt 3666625749) -- the `cmdkey /list`
//!   verification must filter on the DELETED provider's STT credential;
//!   a bare `Select-String "stt-api-key"` gate fails the alternate-provider
//!   escape hatch the same section recommends.
//!   Follow-up `PRRT_kwDOSfNjQs6UsGj3` (P1 on #691, cmt 3672652307): a
//!   colon alone is not enough -- a HARD-CODED provider is reversed for the
//!   other deletion choice the same block offers, so the delete and its
//!   verification are both driven by one `$deleted` variable.
//! * `PRRT_kwDOSfNjQs6UcarV` (P2, cmt 3666625755) -- an outcome the prose
//!   declares a pass must have somewhere to be recorded in the RC template.
//!   The endpoint-marker refusal had no slot; it is now documented as a
//!   FAIL (see the test below for why that, and not a new slot, is right).

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
// Round 7 P2 (`PRRT_kwDOSfNjQs6UcarH`): every `stt_backend` value the README
// hands a tester must be one `AppSettings::validate` accepts.
// ---------------------------------------------------------------------------

/// Byte offset -> 1-based line number, for readable assertion messages.
fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

/// `&text[at-radius ..= at+radius]`, snapped outward to char boundaries so a
/// window that lands inside one of the README's em dashes cannot panic.
fn window(text: &str, at: usize, radius: usize) -> &str {
    let mut start = at.saturating_sub(radius);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (at + radius).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    &text[start..end]
}

/// The `stt_backend` allow-list `AppSettings::validate` really enforces,
/// parsed out of `src/rust/config/validate.rs`.
///
/// Read from the production source on purpose: a hand-copied list in this
/// test would drift the moment a backend is added or renamed, and the whole
/// point is to measure the doc against the CODE.
fn accepted_stt_backend_values() -> Vec<String> {
    let path = repo_root().join("src/rust/config/validate.rs");
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let call = src
        .find("validate_choice(\"stt_backend\"")
        .expect("src/rust/config/validate.rs no longer contains a validate_choice(\"stt_backend\", ...) call -- update this test to follow the renamed validation");
    let tail = &src[call..];
    let open = tail
        .find("&[")
        .expect("validate_choice(\"stt_backend\", ...) has no `&[...]` allow-list literal");
    let close = tail[open..]
        .find(']')
        .expect("unterminated `&[` allow-list in validate_choice(\"stt_backend\", ...)");
    let values: Vec<String> = tail[open + 2..open + close]
        .split(',')
        .map(|raw| raw.trim().trim_matches('"').trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect();
    assert!(
        !values.is_empty(),
        "parsed an empty stt_backend allow-list out of validate.rs"
    );
    values
}

/// Every `(offset, value)` where the README documents an ASSIGNMENT of
/// `stt_backend` -- `` `stt_backend` = `whisper` ``, `stt_backend=whisper`,
/// `stt_backend == "openai"`.
///
/// Requires an actual `=` between the name and the value so prose like
/// "`stt_backend` is required" is not mistaken for an assignment.
fn documented_stt_backend_values(readme: &str) -> Vec<(usize, String)> {
    const NEEDLE: &str = "stt_backend";
    let bytes = readme.as_bytes();
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = readme[from..].find(NEEDLE) {
        let at = from + rel;
        let mut i = at + NEEDLE.len();
        from = i;
        let mut saw_assign = false;
        // `\n` is skipped too so an assignment that wraps across a line
        // (`` `stt_backend` = ``  /  `` `whisper` ``) still resolves.
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\n' | b'`' | b'=' | b'"' | b'\'') {
            saw_assign |= bytes[i] == b'=';
            i += 1;
        }
        if !saw_assign {
            continue;
        }
        let start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-')
        {
            i += 1;
        }
        if i > start {
            found.push((at, readme[start..i].to_owned()));
        }
    }
    found
}

#[test]
fn manual_test_readme_only_documents_valid_stt_backend_values() {
    // Codex P2 PRRT_kwDOSfNjQs6UcarH cmt 3666625739: the step-4 escape hatch
    // told the tester to set `stt_backend` = `local`. `AppSettings::validate`
    // rejects that (`validate_choice("stt_backend", ..., &["whisper",
    // "openai"])`), and the UI's "Local Whisper" option stores `whisper` --
    // so following the doc produced an invalid config instead of the
    // startable backend the step-4 utterance needs.
    let readme = read_manual_test_readme();
    let allowed = accepted_stt_backend_values();
    let documented = documented_stt_backend_values(&readme);
    assert!(
        !documented.is_empty(),
        "manual-test README documents no `stt_backend` value at all; the \
         step-4 escape hatch must name the config value the tester should set \
         -- Codex P2 PRRT_kwDOSfNjQs6UcarH cmt 3666625739."
    );
    for (offset, value) in &documented {
        assert!(
            allowed.iter().any(|candidate| candidate == value),
            "manual-test README line {}: documents `stt_backend` = `{value}`, \
             which `AppSettings::validate` REJECTS -- it accepts only \
             {allowed:?} (`src/rust/config/validate.rs`). A tester who follows \
             this ends up with a config that fails validation instead of a \
             working backend -- Codex P2 PRRT_kwDOSfNjQs6UcarH cmt 3666625739.",
            line_of(&readme, *offset)
        );
    }
    assert!(
        documented.iter().any(|(_, value)| value == "whisper"),
        "the local-Whisper escape hatch must spell out the config value \
         (`stt_backend` = `whisper`) so the tester does not guess `local` \
         -- Codex P2 PRRT_kwDOSfNjQs6UcarH cmt 3666625739."
    );
}

// ---------------------------------------------------------------------------
// Round 7 P2 (`PRRT_kwDOSfNjQs6UcarQ`): the STT-credential verification must
// name the DELETED provider, not every stt-api-key entry.
// ---------------------------------------------------------------------------

/// Every PowerShell variable (`$name` / `${name}`) referenced in `line`.
fn ps_variables(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut names = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        i += 1;
        let braced = i < bytes.len() && bytes[i] == b'{';
        if braced {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if i > start {
            names.push(line[start..i].to_owned());
        }
    }
    names
}

/// The qualifier each `Select-String ... stt-api-key:<qualifier>` filter in
/// the README uses, as `(line number, qualifier, whole line)`. A filter with
/// no `:` at all yields `None` for the qualifier.
fn stt_credential_filters(readme: &str) -> Vec<(usize, Option<String>, &str)> {
    let mut found = Vec::new();
    for (idx, line) in readme.lines().enumerate() {
        let Some((_, after)) = line.split_once("Select-String") else {
            continue;
        };
        let mut from = 0usize;
        while let Some(rel) = after[from..].find("stt-api-key") {
            let end = from + rel + "stt-api-key".len();
            from = end;
            let rest = &after[end..];
            let qualifier = rest.strip_prefix(':').map(|tail| {
                tail.chars()
                    .take_while(|c| !matches!(c, '"' | '\'' | '`' | ' ' | '\t'))
                    .collect::<String>()
            });
            found.push((idx + 1, qualifier, line));
        }
    }
    found
}

#[test]
fn manual_test_readme_stt_credential_check_is_scoped_to_the_deleted_provider() {
    // The alternate-provider escape hatch documented in the same step
    // deliberately KEEPS the other provider's STT credential (e.g. OpenAI
    // STT while post-processing through Groq). A blanket
    // `cmdkey /list | Select-String "stt-api-key"` that "must return
    // NOTHING" -- and the template line that requires it empty -- makes that
    // valid setup unable to pass the Windows release gate.
    let readme = read_manual_test_readme();
    let filters = stt_credential_filters(&readme);
    for (lineno, qualifier, line) in &filters {
        let Some(qualifier) = qualifier else {
            panic!(
                "manual-test README line {lineno}: filters on a bare \
                 `stt-api-key`. The check must be qualified with the deleted \
                 provider (`stt-api-key:$deleted`, \
                 `stt-api-key:<deleted-provider>`): the alternate-provider \
                 escape hatch in the same step keeps the OTHER provider's STT \
                 credential, so an unqualified \"must return NOTHING\" gate \
                 fails a valid setup -- Codex P2 PRRT_kwDOSfNjQs6UcarQ cmt \
                 3666625749.\noffending line: {line}"
            );
        };
        // Codex P1 #691 PRRT_kwDOSfNjQs6UsGj3 cmt 3672652307: a colon is not
        // enough. A HARD-CODED provider is correct for only one of the two
        // deletion choices the same block offers -- a tester who deletes
        // OpenAI would be told to prove the GROQ entry is absent while the
        // deleted OpenAI credential survives, and
        // `resolve_post_api_key`'s same-provider STT fallback would then mask
        // a broken `post-api-key:openai` readback. So the qualifier must be a
        // variable (`$deleted`) or a fill-in placeholder
        // (`<deleted-provider>`), never a literal provider name.
        let acceptable =
            qualifier.is_empty() || qualifier.starts_with('$') || qualifier.starts_with('<');
        assert!(
            acceptable,
            "manual-test README line {lineno}: the STT-credential filter \
             hard-codes the provider (`stt-api-key:{qualifier}`). The block \
             lets the tester delete EITHER provider, so a fixed name is \
             reversed for the other choice: it would demand the wrong \
             credential be absent and let the DELETED one survive, where \
             `resolve_post_api_key`'s same-provider STT fallback masks a \
             broken `post-api-key:<provider>` readback and falsely passes the \
             release gate. Use the `$deleted` variable (or a \
             `<deleted-provider>` placeholder in the template) -- Codex P1 \
             #691 PRRT_kwDOSfNjQs6UsGj3 cmt 3672652307.\noffending line: {line}"
        );
    }

    // The delete and its verification must be driven by the SAME variable,
    // or they can drift apart exactly as the hard-coded pair did.
    let delete_line = readme
        .lines()
        .find(|line| line.trim_start().starts_with("cmdkey /delete:"))
        .expect(
            "manual-test README no longer runs a `cmdkey /delete:` command in \
             step 4 -- Codex P1 PRRT_kwDOSfNjQs6Uajz7 cmt 3665921389",
        );
    let delete_vars = ps_variables(delete_line);
    let verify_line = readme
        .lines()
        .find(|line| line.contains("Select-String") && line.contains("must return NOTHING"))
        .expect(
            "manual-test README lost the `must return NOTHING` verification \
             of the deleted STT credential",
        );
    let shared = ps_variables(verify_line)
        .into_iter()
        .any(|name| delete_vars.contains(&name));
    assert!(
        shared,
        "the `cmdkey /delete:` command and its `must return NOTHING` \
         verification must reference the SAME provider variable, otherwise \
         they can disagree about which credential was deleted -- Codex P1 \
         #691 PRRT_kwDOSfNjQs6UsGj3 cmt 3672652307.\n\
         delete: {delete_line}\nverify: {verify_line}"
    );

    assert!(
        !filters.is_empty(),
        "manual-test README no longer verifies the deleted STT credential \
         with `cmdkey /list | Select-String stt-api-key:<provider>`; the \
         delete must still be proven to have taken effect -- Codex P1 \
         PRRT_kwDOSfNjQs6Uajz7 / P2 PRRT_kwDOSfNjQs6UcarQ."
    );
    assert!(
        readme.contains("PRRT_kwDOSfNjQs6UcarQ"),
        "manual-test README missing the Codex P2 thread cite explaining why \
         the credential check is provider-scoped."
    );
}

// ---------------------------------------------------------------------------
// Round 7 P2 (`PRRT_kwDOSfNjQs6UcarV`): an outcome the prose calls a pass
// must have somewhere to be recorded in the RC template.
// ---------------------------------------------------------------------------

/// The endpoint-marker guard's refusal message
/// (`require_endpoint_matches_marker` / `endpoint_marker_mismatch`).
const ENDPOINT_REFUSAL: &str = "refusing to send stored post-processing key";

/// Prose (everything before the recording template) and the template itself.
fn split_prose_and_template(readme: &str) -> (&str, &str) {
    let idx = readme
        .find("### Recording template")
        .expect("manual-test README missing the `### Recording template` heading");
    (&readme[..idx], &readme[idx..])
}

/// Collapse every whitespace run to a single space.
///
/// The README hard-wraps prose at ~72 columns, so a quoted worker message
/// like the endpoint-marker refusal is split across lines. Matching on the
/// raw text would silently find nothing and turn these tests into no-ops.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The step-4b evidence alternatives inside the recording template.
fn step_4b_evidence_block(template: &str) -> &str {
    let start = template
        .find("- Step 4b")
        .expect("recording template missing the `- Step 4b` evidence line");
    let rel_end = template[start..]
        .find("- Result:")
        .expect("recording template missing the `- Result:` line after step 4b");
    &template[start..start + rel_end]
}

/// The pass-declaring phrase used within `radius` bytes of `needle`, if any.
fn pass_phrase_near(text: &str, needle: &str, radius: usize) -> Option<String> {
    const PHRASES: [&str; 4] = ["valid pass", "also a pass", "counts as a pass", "is a pass"];
    let mut from = 0usize;
    while let Some(rel) = text[from..].find(needle) {
        let at = from + rel;
        from = at + needle.len();
        let haystack = window(text, at, radius).to_lowercase();
        if let Some(phrase) = PHRASES.iter().find(|phrase| haystack.contains(*phrase)) {
            return Some((*phrase).to_owned());
        }
    }
    None
}

#[test]
fn manual_test_readme_pass_outcomes_are_recordable_in_the_template() {
    // The invariant, stated once: if the prose declares an outcome a PASS,
    // the RC template must have a slot that can record it. The final gate
    // forbids an empty step 4b, so a pass with no slot leaves the operator
    // unable to complete the template for a documented success -- Codex P2
    // PRRT_kwDOSfNjQs6UcarV cmt 3666625755.
    //
    // Deliberately an implication rather than a hard-coded expectation: it
    // holds for EITHER remedy Codex offered (add a refusal slot, or stop
    // calling the refusal a pass), and trips only on the inconsistent
    // combination the README actually shipped.
    let readme = read_manual_test_readme();
    let (prose, template) = split_prose_and_template(&readme);
    let prose_flat = flatten(prose);
    let Some(phrase) = pass_phrase_near(&prose_flat, ENDPOINT_REFUSAL, 400) else {
        return; // Not declared a pass -- nothing to record.
    };
    let block = step_4b_evidence_block(template);
    // Alternatives are `  - ...` bullets; flatten each so a wrapped slot
    // still counts.
    let has_slot = block.split("\n  - ").skip(1).any(|alternative| {
        let flat = flatten(alternative).to_lowercase();
        flat.contains("refus") && flat.contains("<paste>")
    });
    assert!(
        has_slot,
        "manual-test README calls the endpoint-marker refusal a pass \
         (matched phrase: {phrase:?}) but the step-4b evidence block has no \
         alternative that can record it: the history / Rust-card \
         alternatives require `post_fallback=false` with an empty error, the \
         log alternative requires a success line, and the capture \
         alternative requires a 2xx. Since the final gate forbids an empty \
         step 4b, the operator cannot complete the RC template for this \
         documented pass. Add a `<paste>` alternative naming the refusal, or \
         stop classifying it as a pass -- Codex P2 PRRT_kwDOSfNjQs6UcarV cmt \
         3666625755.\nstep-4b block:\n{block}"
    );
}

#[test]
fn manual_test_readme_classifies_endpoint_marker_refusal_as_fail() {
    // Which remedy this repo picked, and why (see the PR body): the guard
    // refuses BEFORE any request and returns a `terminal` fallback envelope
    // (`postprocess/run.rs:113-119`), so the provider round-trip step 4
    // measures never happened. Worse, under the different-provider escape
    // hatch the refusal is the SIGNATURE of the regression step 4 exists to
    // catch: a broken `post-api-key:<provider>` readback leaves
    // `post_api_key_input` empty, `worker_command` mirrors the other
    // provider's STT key (`SttMirror`), the marker binds to the STT
    // endpoint, and the guard refuses because the WRONG key reached the
    // worker. Recording that as a pass would ship the broken readback.
    let readme = read_manual_test_readme();
    let (prose, _) = split_prose_and_template(&readme);
    let prose_flat = flatten(prose);
    let at = prose_flat.find(ENDPOINT_REFUSAL).expect(
        "manual-test README must still tell the operator how to classify an \
         endpoint-marker refusal in step 4b -- Codex P2 \
         PRRT_kwDOSfNjQs6UcarV cmt 3666625755",
    );
    let context = window(&prose_flat, at, 400);
    assert!(
        context.contains("FAIL"),
        "the endpoint-marker refusal must be classified as a FAIL: it is \
         returned before any provider request, and under the \
         different-provider escape hatch it is exactly what a broken \
         `post-api-key:<provider>` readback looks like (mirrored STT key + \
         STT-bound marker) -- Codex P2 PRRT_kwDOSfNjQs6UcarV cmt \
         3666625755.\ncontext:\n{context}"
    );
    assert!(
        pass_phrase_near(&prose_flat, ENDPOINT_REFUSAL, 400).is_none(),
        "the endpoint-marker refusal is still described as a pass -- Codex \
         P2 PRRT_kwDOSfNjQs6UcarV cmt 3666625755.\ncontext:\n{context}"
    );
    assert!(
        readme.contains("PRRT_kwDOSfNjQs6UcarV"),
        "manual-test README missing the Codex P2 thread cite for the \
         refusal-outcome classification."
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
