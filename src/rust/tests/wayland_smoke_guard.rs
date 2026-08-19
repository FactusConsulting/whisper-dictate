//! Verifies the Wayland smoke script's hotkey feature guards.

mod common;

use common::read_wayland_smoke;

#[test]
fn wayland_smoke_pins_nullable_hotkey_focus_contract() {
    let smoke = read_wayland_smoke();

    assert!(smoke.contains("hotkey capture help documents nullable focus attribution"));
    assert!(smoke.contains("guided two-window focus verification is unavailable on Wayland"));
    assert!(smoke.contains("grep -Fq 'nullable `focused`'"));
    assert!(smoke.contains("grep -Fq '`null`'"));
}

#[test]
fn wayland_smoke_hotkey_boot_env_matcher_omits_generic_rdev_wrapper() {
    let smoke = read_wayland_smoke();
    // Only the headless-display error is an expected environment gap.
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
        "hotkey-boot environment matcher must contain `MissingDisplayError`."
    );
    assert!(
        !matcher_line.contains("rdev listener failed to start"),
        "hotkey-boot environment matcher must not accept generic listener failures.\n\
         offending line: {matcher_line}"
    );
}

#[test]
fn wayland_smoke_rebuild_with_guard_exists_and_reads_both_classifiers() {
    let block = extract_rebuild_with_guard();
    for token in ["CMD_SOURCE", "installed"] {
        assert!(
            block.contains(token),
            "hotkey-boot rebuild guard must reference `{token}`.\nguard under test:\n{block}"
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
        "hotkey-boot rebuild guard must fail installed binaries before warning.\nguard under \
         test:\n{block}"
    );
}

#[test]
fn audio_recovery_does_not_skip_hotkey_or_provenance_checks() {
    let smoke = read_wayland_smoke();
    let recovery = smoke
        .find("audio input unavailable, but the in-process runtime stayed alive")
        .expect("audio recovery warning");
    let driver = smoke[recovery..]
        .find("Wayland session resolved driver=")
        .expect("Wayland driver gate must still follow audio recovery");
    let provenance = smoke[recovery..]
        .find("transcribe backend resolved:")
        .expect("provenance gate must still follow audio recovery");

    assert!(driver < provenance);
    assert!(
        !smoke[..recovery]
            .lines()
            .rev()
            .take(4)
            .any(|line| line.trim_start().starts_with("elif grep -Eq")),
        "audio recovery must be a nested diagnostic, not an elif that skips later gates"
    );
}

#[test]
fn wayland_smoke_detect_command_classifies_the_origin_of_a_path_binary() {
    let smoke = read_wayland_smoke();
    assert!(
        smoke.contains("classify_installed_origin() {"),
        "wayland-user-smoke.sh must classify the installed command."
    );
    assert!(
        smoke.contains(r#"CMD_ORIGIN="$(classify_installed_origin "$(command -v wd)")""#),
        "`detect_command` must classify the on-PATH `wd` binary."
    );
}

#[cfg(not(windows))]
#[test]
fn wayland_smoke_hotkey_boot_missing_features_fails_only_on_release_artifacts() {
    // Execute the guard so an inverted condition cannot pass a text-only check.
    let block = extract_rebuild_with_guard();

    let release = run_guard(&block, "installed", "release");
    assert_eq!(
        release, "bad",
        "hotkey-boot rebuild guard must fail a shipped release artifact. \
         Observed verdict: {release}.\nguard under test:\n{block}"
    );

    // The installer builds source installs with the same required features.
    let source_install = run_guard(&block, "installed", "source-install");
    assert_eq!(
        source_install, "bad",
        "hotkey-boot rebuild guard must fail a source install. Observed verdict: \
         {source_install}.\nguard under test:\n{block}"
    );

    let uninstalled = run_guard(&block, "none", "");
    assert_eq!(
        uninstalled, "warn",
        "hotkey-boot rebuild guard must warn when no installed command is present. \
         Observed verdict: {uninstalled}.\nguard under \
         test:\n{block}"
    );
}

#[cfg(not(windows))]
#[test]
fn wayland_smoke_classifies_the_install_rust_ui_wrapper_by_what_it_built_from() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let func = extract_shell_function("classify_installed_origin");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let root = tmp.path();

    // A checkout without a bundled binary is a source install.
    let checkout = root.join("checkout");
    fs::create_dir_all(checkout.join("src/rust")).expect("mkdir checkout");
    fs::write(checkout.join("src/rust/Cargo.toml"), "[package]\n").expect("write Cargo.toml");
    let from_source = write_wrapper(&root.join("from-source"), &checkout);

    // A bundle with `wd` at its root is a release install.
    let bundle = root.join("bundle");
    fs::create_dir_all(&bundle).expect("mkdir bundle");
    let prebuilt = bundle.join("wd");
    fs::write(&prebuilt, "ELF-ish\n").expect("write prebuilt");
    fs::set_permissions(&prebuilt, fs::Permissions::from_mode(0o755)).expect("chmod prebuilt");
    let from_bundle = write_wrapper(&root.join("from-bundle"), &bundle);

    // A raw on-PATH binary is treated as a release artifact.
    let raw = root.join("wd");
    fs::write(&raw, "opaque release payload, no wrapper markers\n").expect("write raw binary");
    fs::set_permissions(&raw, fs::Permissions::from_mode(0o755)).expect("chmod raw");

    // Cargo output is a source install.
    let cargo_built = root.join("target/release/wd");
    fs::create_dir_all(cargo_built.parent().expect("target parent")).expect("mkdir target");
    fs::write(&cargo_built, "dev build").expect("write cargo build");

    // Relative Cargo output must also be classified as a source install.
    let relative_release = std::path::PathBuf::from("target/release/wd");
    let relative_debug = std::path::PathBuf::from("target/debug/wd");

    // Alternate Cargo target directories are development builds too.
    let alt_target_abs = root.join("target-linux/release/wd");
    let alt_target_rel = std::path::PathBuf::from("target-linux/debug/wd");

    // A path component merely containing "target" is not a Cargo target directory.
    let not_a_target_dir = root.join("mytargetapp/release/wd");

    let cases = [
        (
            &from_source,
            "source-install",
            "a wrapper rooted at a checkout without a bundled binary is a source install",
        ),
        (
            &from_bundle,
            "release",
            "a wrapper rooted at a bundled binary is a release install",
        ),
        (
            &raw,
            "release",
            "an unrecognised on-PATH binary defaults to release",
        ),
        (
            &cargo_built,
            "source-install",
            "a binary invoked out of a cargo target dir is a developer build",
        ),
        (
            &relative_release,
            "source-install",
            "a relative release target path is a source install",
        ),
        (
            &relative_debug,
            "source-install",
            "a relative debug target path is a source install",
        ),
        (
            &alt_target_abs,
            "source-install",
            "an alternate absolute Cargo target path is a source install",
        ),
        (
            &alt_target_rel,
            "source-install",
            "an alternate relative Cargo target path is a source install",
        ),
        (
            &not_a_target_dir,
            "release",
            "a partial target directory name is not a Cargo target path",
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
            "classify_installed_origin({}) should be `{want}` -- {why}",
            path.display()
        );
    }
}

/// Write an installer-style wrapper rooted at `app_root`.
#[cfg(not(windows))]
fn write_wrapper(path: &std::path::Path, app_root: &std::path::Path) -> std::path::PathBuf {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let body = format!(
        "#!/usr/bin/env bash\nexport VOICEPI_APP_ROOT=\"{}\"\nexec \"{}/wd-app\" \"$@\"\n",
        app_root.display(),
        app_root.display(),
    );
    fs::write(path, body).expect("write wrapper");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod wrapper");
    path.to_path_buf()
}

// ---------------------------------------------------------------------------
// Extraction helpers.
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
