//! Regression tests for the `scripts/integration/wayland-user-smoke.sh`
//! hotkey-boot guards.
//!
//! Split out of `manual_test_docs.rs` (which keeps the manual-test README
//! findings) so neither file grows past the repo's file-size guidance.
//!
//! Threads pinned here:
//!
//! * `PRRT_kwDOSfNjQs6Uaj0A` (P2, cmt 3665921394) -- the hotkey-boot
//!   environment-gap matcher must NOT include the generic
//!   `rdev listener failed to start` wrapper, only the specific
//!   `MissingDisplayError` token.
//! * `PRRT_kwDOSfNjQs6Uaj0I` (P2, cmt 3665921401) -- the hotkey-boot
//!   rebuild-with branch must fail (`bad`), not skip, for a shipped release
//!   binary (a release artifact must carry both features).
//! * `PRRT_kwDOSfNjQs6Ubpeb` (P2, cmt 3666333668) -- that guard test must
//!   EXECUTE the extracted branch under bash with mocked values, not
//!   pattern-match its text (an inverted `!=` guard passed the old test).
//! * #703 made source installs feature-complete after retiring the Python
//!   runtime, so both prebuilt and source-installed artifacts must now fail
//!   loudly if native hotkey/injection support is absent.
//! * `PRRT_kwDOSfNjQs6UdcEe` (cmt 3667025623) -- gating the whole
//!   bash-executing test to `#[cfg(not(windows))]` (PR #678) also gated the
//!   only extractor of the guard block, leaving the Windows leg with zero
//!   coverage that the guard exists at all. Only the `Command::new("bash")`
//!   call is platform-gated now; extraction plus the structural assertions
//!   run everywhere.

mod common;

use common::read_wayland_smoke;

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
    let after_header = &smoke[hotkey_boot_section_start(&smoke)..];
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
// Cross-platform structure: the rebuild-with guard must EXIST and be wired to
// CMD_SOURCE. Every installed artifact is feature-complete now.
//
// Codex cmt 3667025623: the behavioural test below can only run where `bash`
// is the shell that actually interprets this script, but the Windows CI leg
// must still fail if the guard is deleted or structurally gutted -- this is a
// Windows-shipped release-packaging concern. These assertions are cheap,
// need no subprocess, and run on every platform.
// ---------------------------------------------------------------------------

#[test]
fn wayland_smoke_rebuild_with_guard_exists_and_reads_both_classifiers() {
    let block = extract_rebuild_with_guard();
    for token in ["CMD_SOURCE", "installed"] {
        assert!(
            block.contains(token),
            "hotkey-boot rebuild-with guard must reference `{token}`: a \
             any installed artifact missing rust-hotkeys / rust-injection is \
             a packaging regression (`bad`); only ad-hoc source execution may \
             warn-skip.\nguard under test:\n{block}"
        );
    }
    let bad_at = block
        .find("bad \"")
        .expect("hotkey-boot rebuild-with guard must still call `bad` for installed artifacts");
    let warn_at = block
        .find("warn \"")
        .expect("hotkey-boot rebuild-with guard must still call `warn` for ad-hoc source builds");
    assert!(
        bad_at < warn_at,
        "hotkey-boot rebuild-with guard must take the `bad` branch first \
         (the positive release-artifact condition) and fall back to `warn`; \
         a swapped pair means the release-packaging regression warn-skips \
         -- Codex PRRT_kwDOSfNjQs6UdcEe cmt 3667025623.\nguard under \
         test:\n{block}"
    );
}

#[test]
fn wayland_smoke_detect_command_classifies_the_origin_of_a_path_binary() {
    let smoke = read_wayland_smoke();
    assert!(
        smoke.contains("classify_installed_origin() {"),
        "wayland-user-smoke.sh must define `classify_installed_origin`: \
         `CMD_SOURCE=installed` only says a binary is on PATH, and \
         `scripts/linux/install-rust-ui.sh` puts a deliberately \
         reduced-feature SOURCE build there too -- Codex P2 \
         PRRT_kwDOSfNjQs6Ucarb cmt 3666625761."
    );
    assert!(
        smoke.contains(
            r#"CMD_ORIGIN="$(classify_installed_origin "$(command -v whisper-dictate)")""#
        ),
        "`detect_command` must set CMD_ORIGIN from \
         `classify_installed_origin` for the on-PATH binary, or the guard \
         reads a stale/empty origin -- Codex P2 PRRT_kwDOSfNjQs6Ucarb cmt \
         3666625761."
    );
}

// ---------------------------------------------------------------------------
// P2: rebuild-with branch must FAIL only for a shipped release artifact.
// ---------------------------------------------------------------------------

// Executes the guard under `bash`, so THIS TEST is scoped to the platforms
// the script itself targets -- see the comment on `run_bash` for why. The
// structural assertions above stay cross-platform.
#[cfg(not(windows))]
#[test]
fn wayland_smoke_hotkey_boot_missing_features_fails_only_on_release_artifacts() {
    // Codex P2 PRRT_kwDOSfNjQs6Ubpeb cmt 3666333668: the earlier version
    // of this test only checked that the surrounding text CONTAINED
    // `CMD_SOURCE` / `installed` and that some `bad` preceded some
    // `warn`. Inverting the guard to
    // `if [ "$CMD_SOURCE" != "installed" ]` -- i.e. exactly restoring
    // the release-binary warn-skip bug -- satisfied every one of those
    // assertions. So instead of pattern-matching the source, EXECUTE the
    // extracted guard under bash with `bad` / `warn` stubbed, once per
    // classification, and assert the OBSERVED verdict. An inverted
    // guard now flips the verdicts and trips the test.
    let block = extract_rebuild_with_guard();

    let release = run_guard(&block, "installed", "release");
    assert_eq!(
        release, "bad",
        "hotkey-boot rebuild-with guard must call `bad` for a shipped \
         release artifact (CMD_SOURCE=installed, CMD_ORIGIN=release): the \
         release workflow builds it with rust-hotkeys + rust-injection, so \
         a rebuild-with message from one is a packaging regression, not an \
         environment gap. Observed verdict: {release} -- Codex P2 \
         PRRT_kwDOSfNjQs6Uaj0I cmt 3665921401 / PRRT_kwDOSfNjQs6Ubpeb cmt \
         3666333668.\nguard under test:\n{block}"
    );

    // Codex P2 PRRT_kwDOSfNjQs6Ucarb cmt 3666625761: a source install lands
    // on PATH exactly like a release artifact (CMD_SOURCE=installed), but
    // `install-rust-ui.sh` now builds the complete native route, so a missing
    // hotkey feature in a source install is also a packaging failure.
    let source_install = run_guard(&block, "installed", "source-install");
    assert_eq!(
        source_install, "bad",
        "hotkey-boot rebuild-with guard must fail for a source install because \
         scripts/linux/install-rust-ui.sh builds the full native feature set. \
         Observed verdict: \
         {source_install} -- Codex P2 PRRT_kwDOSfNjQs6Ucarb cmt \
         3666625761.\nguard under test:\n{block}"
    );

    // The Python dev fallback never claimed to be the shipping binary
    // either. Asserting a third case is what makes an inverted guard
    // detectable: a `!=` flip keeps "some bad and some warn exist" true
    // while swapping the verdicts.
    let python_fallback = run_guard(&block, "source", "");
    assert_eq!(
        python_fallback, "warn",
        "hotkey-boot rebuild-with guard must warn-skip (not fail) for the \
         source/Python dev fallback. Observed verdict: {python_fallback} \
         -- Codex P2 PRRT_kwDOSfNjQs6Ubpeb cmt 3666333668.\nguard under \
         test:\n{block}"
    );
}

// ---------------------------------------------------------------------------
// P2 (cmt 3666625761): the origin classifier itself.
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
#[test]
fn wayland_smoke_classifies_the_install_rust_ui_wrapper_by_what_it_built_from() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let func = extract_shell_function("classify_installed_origin");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let root = tmp.path();

    // (1) A repo checkout: `install-rust-ui.sh` finds `src/rust/Cargo.toml`
    //     and no prebuilt binary, so it compiles the complete native route.
    let checkout = root.join("checkout");
    fs::create_dir_all(checkout.join("src/rust")).expect("mkdir checkout");
    fs::write(checkout.join("src/rust/Cargo.toml"), "[package]\n").expect("write Cargo.toml");
    let from_source = write_wrapper(&root.join("from-source"), &checkout);

    // (2) An unpacked release bundle: `release.yml` ships the prebuilt
    //     binary at the bundle root and NO `src/rust`, so the installer
    //     copies the shipped artifact verbatim.
    let bundle = root.join("bundle");
    fs::create_dir_all(&bundle).expect("mkdir bundle");
    let prebuilt = bundle.join("wd");
    fs::write(&prebuilt, "ELF-ish\n").expect("write prebuilt");
    fs::set_permissions(&prebuilt, fs::Permissions::from_mode(0o755)).expect("chmod prebuilt");
    let from_bundle = write_wrapper(&root.join("from-bundle"), &bundle);

    // (3) A raw release binary dropped on PATH (no wrapper at all).
    let raw = root.join("wd");
    fs::write(&raw, "opaque release payload, no wrapper markers\n").expect("write raw binary");
    fs::set_permissions(&raw, fs::Permissions::from_mode(0o755)).expect("chmod raw");

    // (4) A developer running straight out of the cargo target dir.
    let cargo_built = root.join("target/release/wd");
    fs::create_dir_all(cargo_built.parent().expect("target parent")).expect("mkdir target");
    fs::write(&cargo_built, "dev build").expect("write cargo build");

    // (5) The same developer build reached through a RELATIVE PATH entry
    //     (`PATH=target/release:$PATH`), where `command -v` returns
    //     `target/release/wd` with no leading slash. This case
    //     is purely lexical -- the classifier's `case` fires before any
    //     filesystem test -- so no file has to exist for it.
    let relative_release = std::path::PathBuf::from("target/release/wd");
    let relative_debug = std::path::PathBuf::from("target/debug/wd");

    // (6) This repo's own alternate `--target-dir`: `scripts/dev/dev-check.ps1`
    //     builds into `target-linux/`, and its release leg uses
    //     `whisper-rs-local` WITHOUT rust-hotkeys / rust-injection -- so that
    //     binary on PATH must classify as a dev build, absolute or relative.
    let alt_target_abs = root.join("target-linux/release/wd");
    let alt_target_rel = std::path::PathBuf::from("target-linux/debug/wd");

    // (7) The guard must not over-reach: a directory that merely CONTAINS
    //     "target" in its name is not a cargo target dir.
    let not_a_target_dir = root.join("mytargetapp/release/wd");

    let cases = [
        (
            &from_source,
            "source-install",
            "a wrapper whose app root carries src/rust/Cargo.toml and no \
             prebuilt binary was compiled by install-rust-ui.sh",
        ),
        (
            &from_bundle,
            "release",
            "a wrapper whose app root carries the prebuilt bundle binary \
             installed the SHIPPED artifact",
        ),
        (
            &raw,
            "release",
            "an unrecognised on-PATH binary must default to `release` so a \
             genuinely broken release still fails loudly",
        ),
        (
            &cargo_built,
            "source-install",
            "a binary invoked out of a cargo target dir is a developer build",
        ),
        (
            &relative_release,
            "source-install",
            "`PATH=target/release:$PATH` makes `command -v` return a bare \
             relative path with no leading slash; a slash-anchored pattern \
             alone would misfile that reduced-feature dev build as a release \
             artifact and turn its expected hotkey skip into a hard failure \
             (Codex P2 #692 cmt 3672864372)",
        ),
        (
            &relative_debug,
            "source-install",
            "same for the bare relative debug path (Codex P2 #692 cmt 3672864372)",
        ),
        (
            &alt_target_abs,
            "source-install",
            "this repo's own `--target-dir` is `target-linux/` \
             (`scripts/dev/dev-check.ps1`), whose release leg builds \
             whisper-rs-local WITHOUT rust-hotkeys / rust-injection -- an \
             exact `target/` match would report that dev binary as a \
             packaging failure (Codex P2 #692 cmt 3672959936)",
        ),
        (
            &alt_target_rel,
            "source-install",
            "same for the relative `target-linux/debug` form (Codex P2 #692 \
             cmt 3672959936)",
        ),
        (
            &not_a_target_dir,
            "release",
            "a directory that merely contains the word `target` in its name \
             is not a cargo target dir; the pattern must stay anchored to a \
             path component that STARTS with `target`",
        ),
    ];

    for (path, want, why) in cases {
        let script = format!(
            "set -u\n{func}\nclassify_installed_origin '{}'\n",
            path.display()
        );
        let got = run_bash(&script);
        assert_eq!(
            got,
            want,
            "classify_installed_origin({}) should be `{want}` -- {why} \
             (Codex P2 PRRT_kwDOSfNjQs6Ucarb cmt 3666625761)",
            path.display()
        );
    }
}

/// Write the shell wrapper `scripts/linux/install-rust-ui.sh:48-53` installs,
/// pointing at `app_root`, and return its path.
#[cfg(not(windows))]
fn write_wrapper(path: &std::path::Path, app_root: &std::path::Path) -> std::path::PathBuf {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let body = format!(
        "#!/usr/bin/env bash\nexport VOICEPI_APP_ROOT=\"{}\"\nexec \"{}/whisper-dictate-app\" \"$@\"\n",
        app_root.display(),
        app_root.display(),
    );
    fs::write(path, body).expect("write wrapper");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod wrapper");
    path.to_path_buf()
}

// ---------------------------------------------------------------------------
// Extraction helpers -- deliberately cross-platform (Codex cmt 3667025623).
// ---------------------------------------------------------------------------

fn hotkey_boot_section_start(smoke: &str) -> usize {
    let header = "self-test hotkey-boot (Windows PTT-boot regression";
    smoke
        .find(header)
        .expect("wayland-user-smoke.sh missing the self-test hotkey-boot section header")
}

/// Slice the literal `if [ ... ] ... fi` block out of the hotkey-boot
/// rebuild-with branch so it can be inspected and executed verbatim.
///
/// Deliberately extracts by structure (the `if`/`fi` lines inside the located
/// branch) rather than matching the guard's text, so the test cannot pass by
/// "recognising" the correct condition -- whatever condition is actually there
/// gets run.
fn extract_rebuild_with_guard() -> String {
    let smoke = read_wayland_smoke();
    let start = hotkey_boot_section_start(&smoke);
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

/// Slice a whole `name() { ... }` shell function out of the smoke script.
#[cfg(not(windows))]
fn extract_shell_function(name: &str) -> String {
    let smoke = read_wayland_smoke();
    let opener = format!("{name}() {{");
    let start = smoke
        .find(&opener)
        .unwrap_or_else(|| panic!("wayland-user-smoke.sh: missing shell function `{name}`"));

    let mut lines = Vec::new();
    for line in smoke[start..].lines() {
        lines.push(line);
        if line == "}" {
            return lines.join("\n");
        }
    }
    panic!("wayland-user-smoke.sh: unterminated shell function `{name}`");
}

// ---------------------------------------------------------------------------
// bash execution -- the ONLY part that is platform-gated.
//
// `wayland-user-smoke.sh` only ever runs on Linux (the user's Ubuntu Wayland
// box and the ubuntu-2604 integration container); the Windows CI leg has no
// Wayland session and its `bash` is whatever Git-for-Windows/MSYS ships,
// whose exit-status and pipeline semantics differ from the shell that will
// actually interpret this script. Running it there produced a non-zero exit
// with empty stderr on windows-2025 while passing on both Linux CI and a
// local Git-bash -- i.e. pure runner noise about a file Windows never
// executes.
// ---------------------------------------------------------------------------

/// Execute `block` under bash with the script's `bad` / `warn` reporters
/// stubbed to echo their own name, for the given classification.
///
/// Returns the verdict the guard actually reached: `"bad"`, `"warn"`, or
/// `"(none)"` if it took neither branch.
#[cfg(not(windows))]
fn run_guard(block: &str, cmd_source: &str, cmd_origin: &str) -> String {
    // `hb_out` is referenced inside the real `bad`/`warn` messages via a
    // command substitution, so it must exist or `set -u` semantics in the
    // harness would abort. Give it a realistic rebuild-with payload.
    let script = format!(
        r#"set -u
CMD_SOURCE="{cmd_source}"
CMD_ORIGIN="{cmd_origin}"
hb_out="error: rust-hotkeys feature is not compiled in (rebuild with --features rust-hotkeys)"
bad()  {{ printf 'bad\n'; }}
warn() {{ printf 'warn\n'; }}
{block}
"#
    );
    let verdict = run_bash(&script);
    if verdict.is_empty() {
        "(none)".to_owned()
    } else {
        verdict
    }
}

/// Run `script` under bash, assert it exited zero, and return trimmed stdout.
#[cfg(not(windows))]
fn run_bash(script: &str) -> String {
    use std::process::Command;

    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .output()
        .expect("run the extracted shell under bash (bash must be on PATH)");
    assert!(
        out.status.success(),
        "extracted shell exited non-zero\nstderr: {}\nscript:\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
