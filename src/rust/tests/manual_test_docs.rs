//! Regression tests for the manual-test README and the wayland-user-smoke
//! shell script.
//!
//! These pin the four Codex findings on PR #672 that live in text-only
//! artifacts (docs + shell), so a future edit that reverts the substantive
//! fix trips a red test rather than being caught only by the next Codex
//! round.
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

#[test]
fn wayland_smoke_hotkey_boot_missing_features_fails_on_installed_release() {
    let smoke = read_wayland_smoke();
    // Find the rebuild-with branch (the elif for `rust-hotkeys|
    // rust-injection|rebuild with`) and verify that it now checks
    // `CMD_SOURCE = installed` -> `bad`, so a packaging regression on
    // the shipped binary cannot silently downgrade to `warn`.
    let header = "self-test hotkey-boot (Windows PTT-boot regression";
    let start = smoke
        .find(header)
        .expect("wayland-user-smoke.sh missing the self-test hotkey-boot section header");
    let after_header = &smoke[start..];
    // Grab a generous window covering the whole `if/elif/else` block so
    // both the guard and the `bad` call are in scope.
    let window: String = after_header.chars().take(6000).collect();
    let rebuild_idx = window.find("rebuild with").expect(
        "wayland-user-smoke.sh: expected a `rebuild with` guard in the \
         hotkey-boot section",
    );
    let branch = &window[rebuild_idx..];
    assert!(
        branch.contains("CMD_SOURCE") && branch.contains("installed"),
        "hotkey-boot rebuild-with branch must gate on `CMD_SOURCE = \
         installed` so an installed release binary that ships without \
         `rust-hotkeys` / `rust-injection` is treated as `bad`, not \
         `warn` -- Codex P2 PRRT_kwDOSfNjQs6Uaj0I cmt 3665921401."
    );
    // And the branch must actually call `bad` for the installed case
    // (not just log info). Look for `bad "hotkey-boot FAILED` after the
    // CMD_SOURCE check, before the fallback `warn`.
    let cmd_idx = branch
        .find("CMD_SOURCE")
        .expect("just asserted CMD_SOURCE is present");
    let after_cmd = &branch[cmd_idx..];
    let bad_pos = after_cmd
        .find("bad \"")
        .expect("hotkey-boot rebuild-with branch must call `bad ...` after the CMD_SOURCE guard");
    let warn_pos = after_cmd.find("warn \"").unwrap_or(usize::MAX);
    assert!(
        bad_pos < warn_pos,
        "hotkey-boot rebuild-with branch must call `bad ...` BEFORE the \
         `warn ...` fallback so the installed-release case fails hard \
         -- Codex P2 PRRT_kwDOSfNjQs6Uaj0I cmt 3665921401."
    );
}
