#!/usr/bin/env pwsh
# Run cargo fmt --check + clippy + test in the devcontainer image, against
# the Linux target (x86_64-unknown-linux-gnu). Mirrors CI's
# `rust (ubuntu-latest)` job so "green here" == "green in CI" for the lint
# + test surface. Bypasses the native Windows MSVC/lib.exe toolchain
# (which is broken on this dev box per the memory note).
#
# Usage from any worktree (run from PowerShell -- Docker Desktop must
# be running and the `desktop-linux` context must be selectable):
#
#   pwsh scripts/dev/dev-check.ps1                  # all CI feature legs
#   pwsh scripts/dev/dev-check.ps1 -DryRun          # print commands, no exec
#   pwsh scripts/dev/dev-check.ps1 -SkipExtraFeatures   # only ui-egui-glow leg
#
# Requires:
#   - Docker Desktop running (any recent version exposes desktop-linux)
#
# The image is built once on first run (~5 min); thereafter Docker's
# layer cache keeps subsequent runs near-instant. Earlier revisions of
# this script routed through Rancher Desktop's WSL distro, but the
# distro kept silently unregistering itself mid-session
# ("WSL_E_DISTRO_NOT_FOUND"); Docker Desktop's stable named-pipe
# daemon endpoint sidesteps that failure mode.

param(
    [switch]$DryRun,
    [switch]$SkipExtraFeatures,
    # Backwards-compat shim: the original script took `-Features <list>`
    # and ran a single cargo test pass with that feature list. The
    # rewrite drives the full CI matrix unconditionally, so this
    # parameter is now ignored -- accepted only to avoid a
    # parameter-binding error for invocations baked into AGENTS.md /
    # local muscle memory. Codex P2 #418 dev-check.ps1 ($-Features
    # entry point). Remove once AGENTS.md and the developer team have
    # migrated.
    [string]$Features
)

$ErrorActionPreference = "Stop"

$repoRoot = (& git rev-parse --show-toplevel | ForEach-Object { $_.Trim() })
if (-not $repoRoot) { throw "Not inside a git repo." }
# Git emits forward slashes; docker.exe wants the native Windows form
# for its -v mount. Docker Desktop translates `D:\...` to the Linux
# mount automatically.
$repoRoot = $repoRoot -replace '/', '\'

# Docker Desktop's `desktop-linux` context is the daemon endpoint we
# target. Pinning the context explicitly keeps the script working
# regardless of whatever `docker context use` the developer last ran.
$DockerContext = 'desktop-linux'
Write-Host "[dev-check] repo at $repoRoot (mounted into devcontainer via Docker Desktop)" -ForegroundColor Cyan

# ---- CI parity matrix ------------------------------------------------------
#
# The Ubuntu rust job in .github/workflows/test.yml runs FOUR Rust
# invocations: one clippy + a default `cargo test` + a feature-gated
# test for `rust-hotkeys` + a feature-gated test for `audio-in-rust`.
# Running only the default leg locally lets a feature-gated regression
# slip through to CI -- Codex P2 #418 dev-check.ps1:121. So this
# wrapper drives the same four legs by default. `-SkipExtraFeatures`
# limits it to the ui-egui-glow leg for fast local iteration where the
# developer knows the feature paths are untouched.
#
# Centralising the cargo argv lists in one place also gives us a
# testable surface: a smoke test can dot-source this script with
# `-DryRun` set and assert the printed argv matches the expected
# matrix (Codex P2 #418 dev-check.ps1:51).

function Get-CargoLegs {
    [OutputType([hashtable[]])]
    param([switch]$IncludeExtraFeatures)
    $legs = @(
        @{
            Name = 'cargo fmt --all -- --check'
            Argv = @('cargo', 'fmt', '--manifest-path', 'src/rust/Cargo.toml', '--all', '--', '--check')
        },
        @{
            Name = 'cargo clippy (ui-egui-glow)'
            Argv = @(
                'cargo', 'clippy',
                '--manifest-path', 'src/rust/Cargo.toml',
                '--target-dir', 'target-linux',
                '-p', 'whisper-dictate-app',
                '--all-targets',
                '--features', 'ui-egui-glow',
                '--', '-D', 'warnings'
            )
        },
        @{
            Name = 'cargo test (default)'
            Argv = @(
                'cargo', 'test',
                '--manifest-path', 'src/rust/Cargo.toml',
                '--target-dir', 'target-linux',
                '-p', 'whisper-dictate-app'
            )
        }
    )
    if ($IncludeExtraFeatures) {
        $legs += @{
            Name = 'cargo test --features rust-hotkeys'
            Argv = @(
                'cargo', 'test',
                '--manifest-path', 'src/rust/Cargo.toml',
                '--target-dir', 'target-linux',
                '-p', 'whisper-dictate-app',
                '--features', 'rust-hotkeys'
            )
        }
        $legs += @{
            Name = 'cargo test --features audio-in-rust'
            Argv = @(
                'cargo', 'test',
                '--manifest-path', 'src/rust/Cargo.toml',
                '--target-dir', 'target-linux',
                '-p', 'whisper-dictate-app',
                '--features', 'audio-in-rust'
            )
        }
        # CIs Rust job runs `cargo build ... --features whisper-rs-local --release`
        # (test.yml:360-371) as a compile-only smoke for the whisper.cpp /
        # whisper-rs link boundary. Mirror it here so a whisper-rs API break
        # surfaces before tagged releases. `cargo test` is overkill (whisper-rs
        # tests want a GGML fixture we don't ship), so we use `cargo build`
        # to match CI exactly. Codex P2 #418 dev-check.ps1:121 round 2.
        $legs += @{
            Name = 'cargo build --features whisper-rs-local --release'
            Argv = @(
                'cargo', 'build',
                '--manifest-path', 'src/rust/Cargo.toml',
                '--target-dir', 'target-linux',
                '-p', 'whisper-dictate-app',
                '--features', 'whisper-rs-local',
                '--release'
            )
        }
    }
    return $legs
}

# When dot-sourced for tests, expose the function and exit before any
# docker side-effects.
if ($DryRun) {
    $legs = Get-CargoLegs -IncludeExtraFeatures:(-not $SkipExtraFeatures)
    foreach ($leg in $legs) {
        Write-Host "[dry-run] $($leg.Name)" -ForegroundColor Yellow
        Write-Host ('  ' + ($leg.Argv -join ' '))
    }
    return
}

# Always invoke `docker build` -- it's the only way to pick up changes to
# .devcontainer/Dockerfile or .devcontainer/devcontainer.json on an
# already-existing image. Docker's layer cache makes a no-op rebuild
# near-instant (~1-2 s when nothing changed); a stale image is the more
# expensive failure mode (silently divergent toolchain vs CI). Codex
# P2 #414 dev-check.ps1:50 (the previous if-missing guard would diverge
# from CI whenever Dockerfile changed in another worktree).
Write-Host "[dev-check] docker build (layer cache makes no-op rebuild fast)..." -ForegroundColor Cyan
docker --context $DockerContext build -t whisper-dictate-dev:latest "$repoRoot\.devcontainer\"
if ($LASTEXITCODE -ne 0) { throw "image build failed" }

function Invoke-InContainer([string[]]$cmd) {
    # Mount the repo at /repo (read-write for target dir + .cargo-cache).
    # Use a Linux-side `target-linux/` so the Linux artefacts don't collide
    # with native Windows ones in `target/`.
    #
    # Persist both CARGO_HOME and RUSTUP_HOME on the host so the pinned
    # toolchain (rust-toolchain.toml -> 1.96.0) installed by rustup on
    # first use survives across `docker run --rm` invocations.
    # `docker run --rm` does NOT run the devcontainer `postCreateCommand`,
    # so the toolchain is otherwise downloaded fresh every call. Codex
    # P2 #414 dev-check.ps1:63.
    #
    # The caller passes the cargo command as a string array so the
    # `--features <list>` flag survives bash word-splitting -- a quoted
    # `"rust-hotkeys audio-in-rust"` was previously interpolated into
    # `bash -lc` as a bare word and split before cargo received it.
    # Codex P2 #414 dev-check.ps1:74.
    $args = @(
        '--context', $DockerContext,
        'run', '--rm',
        '-v', "${repoRoot}:/repo",
        '-w', '/repo',
        '-e', 'CARGO_HOME=/repo/.cargo-cache',
        '-e', 'RUSTUP_HOME=/repo/.rustup-cache',
        'whisper-dictate-dev:latest'
    ) + $cmd
    & docker @args
    if ($LASTEXITCODE -ne 0) { throw "container command failed: $($cmd -join ' ')" }
}

$legs = Get-CargoLegs -IncludeExtraFeatures:(-not $SkipExtraFeatures)
foreach ($leg in $legs) {
    Write-Host "[dev-check] $($leg.Name)" -ForegroundColor Cyan
    Invoke-InContainer $leg.Argv
}

Write-Host "[dev-check] OK -- ready to push" -ForegroundColor Green
