//! Vulkan SDK install policy for the Windows release build.
//!
//! `windows-installer-build.yml` installs the pinned LunarG Vulkan SDK through
//! `scripts/windows/install-vulkan-sdk.ps1`: a SHA-256-verified download from
//! the repository's toolchain mirror release with sdk.lunarg.com as fallback,
//! each attempt capped by a step timeout, and one retry. These checks pin that
//! shape so an SDK download stall fails fast instead of holding the release.

mod common;

use std::fs;

const WORKFLOW: &str = ".github/workflows/windows-installer-build.yml";
const SCRIPT: &str = "scripts/windows/install-vulkan-sdk.ps1";
const FIRST_ATTEMPT: &str = "Install Vulkan SDK (Windows GPU acceleration for whisper.cpp)";
const RETRY: &str = "Retry Vulkan SDK install";
const KILL_SWITCH: &str =
    "if: steps.win-check.outputs.changed == 'true' && env.VOICEPI_BUILD_VULKAN != '0'";
const INVOKE: &str = "& $env:RELEASE_VULKAN_SDK_INSTALLER `
            -Version $env:VULKAN_SDK_VERSION `
            -Sha256 $env:VULKAN_SDK_SHA256 `
            -MirrorUrl $env:VULKAN_SDK_MIRROR_URL";

fn read_repo(path: &str) -> String {
    let full = common::repo_root().join(path);
    fs::read_to_string(&full)
        .unwrap_or_else(|error| panic!("read {}: {error}", full.display()))
        .replace("\r\n", "\n")
}

/// Byte offset and body of the step named `name`, up to the next step.
fn step(workflow: &str, name: &str) -> (usize, String) {
    let header = format!("      - name: {name}\n");
    let start = workflow
        .find(&header)
        .unwrap_or_else(|| panic!("step {name:?} missing"));
    let rest = &workflow[start + header.len()..];
    let end = rest.find("\n      - ").unwrap_or(rest.len());
    (start, rest[..end].to_owned())
}

/// Value of the job-level `KEY: value` env entry.
fn env_value(workflow: &str, key: &str) -> String {
    let prefix = format!("      {key}: ");
    workflow
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("env {key} missing"))
        .trim()
        .to_owned()
}

#[test]
fn vulkan_sdk_install_is_time_boxed_with_one_retry() {
    let workflow = read_repo(WORKFLOW);
    assert!(
        !workflow.contains("jakoch/install-vulkan-sdk-action"),
        "the SDK install must go through the checksum-verified mirror script"
    );

    let (first_at, first) = step(&workflow, FIRST_ATTEMPT);
    for required in [
        "id: vulkan-sdk",
        KILL_SWITCH,
        "timeout-minutes: 10",
        "continue-on-error: true",
        INVOKE,
    ] {
        assert!(
            first.contains(required),
            "first SDK attempt is missing {required:?}:\n{first}"
        );
    }

    let (retry_at, retry) = step(&workflow, RETRY);
    let retry_if = format!("{KILL_SWITCH} && steps.vulkan-sdk.outcome != 'success'");
    for required in [retry_if.as_str(), "timeout-minutes: 10", INVOKE] {
        assert!(
            retry.contains(required),
            "SDK retry is missing {required:?}:\n{retry}"
        );
    }
    assert!(
        !retry.contains("continue-on-error"),
        "a failed retry must fail the job before the build runs without an SDK"
    );

    let (build_at, _) = step(&workflow, "Build Rust desktop UI");
    assert!(
        first_at < retry_at && retry_at < build_at,
        "the retry must run right after the first attempt and before the build"
    );
}

#[test]
fn vulkan_sdk_installer_is_checksum_verified_from_the_mirror_first() {
    let workflow = read_repo(WORKFLOW);
    let version = env_value(&workflow, "VULKAN_SDK_VERSION");
    let sha256 = env_value(&workflow, "VULKAN_SDK_SHA256");
    assert_eq!(sha256.len(), 64, "VULKAN_SDK_SHA256 must be a full SHA-256");
    assert!(sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_eq!(
        env_value(&workflow, "VULKAN_SDK_MIRROR_URL"),
        format!(
            "https://github.com/${{{{ github.repository }}}}/releases/download/\
             toolchain-vulkan-sdk-{version}/vulkansdk-windows-X64-{version}.exe"
        ),
        "the mirror release must carry the same SDK version as VULKAN_SDK_VERSION"
    );
    // Manual rebuilds check out older tags, so the script comes from the
    // reviewed build recipe like the other release helpers.
    assert!(workflow.contains("RELEASE_VULKAN_SDK_INSTALLER = 'install-vulkan-sdk.ps1'"));

    let script = read_repo(SCRIPT);
    let mirror = script
        .find("  $MirrorUrl,\n")
        .expect("mirror must be the first download source");
    let lunarg = script
        .find("\"https://sdk.lunarg.com/sdk/download/$Version/windows/$installerName\"")
        .expect("LunarG must remain the fallback source");
    assert!(mirror < lunarg, "the mirror must be tried before LunarG");
    for required in [
        "$actual = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()",
        "if ($actual -ne $Sha256.ToLowerInvariant()) {",
        "if (-not $verified) {",
        "--speed-limit 262144 --speed-time 60",
        "Remove-Item -LiteralPath $Destination -Recurse -Force",
        "'--root', $Destination, '--accept-licenses', '--default-answer', '--confirm-command', 'install'",
        "if ($process.ExitCode -ne 0) {",
        "'Bin\\glslc.exe'",
        "\"VULKAN_SDK=$Destination\" | Add-Content -LiteralPath $env:GITHUB_ENV",
        "(Join-Path $Destination 'Bin') | Add-Content -LiteralPath $env:GITHUB_PATH",
    ] {
        assert!(
            script.contains(required),
            "install-vulkan-sdk.ps1 is missing {required:?}"
        );
    }
    let hash_check = script.find("if ($actual -ne").unwrap();
    let install = script.find("Start-Process -FilePath $installer").unwrap();
    assert!(
        hash_check < install,
        "the installer must only run after its SHA-256 matched"
    );
}
