#!/usr/bin/env pwsh
# Run cargo fmt --check + clippy + test in the devcontainer image, against
# the Linux target (x86_64-unknown-linux-gnu). Mirrors CI's
# `rust (ubuntu-latest)` job so "green here" == "green in CI" for the lint
# + test surface. Bypasses the native Windows MSVC/lib.exe toolchain
# (which is broken on this dev box per the memory note).
#
# Usage from any worktree (run from PowerShell — Rancher Desktop's WSL
# distro hosts the daemon, but the named pipe isn't exposed to Windows,
# so docker.exe from the Windows side can't connect):
#
#   pwsh scripts/dev/dev-check.ps1                       # default features
#   pwsh scripts/dev/dev-check.ps1 -Features audio-in-rust
#
# Requires:
#   - Rancher Desktop running (Settings -> WSL Integration -> rancher-desktop on)
#   - WSL 2 with the `rancher-desktop` distro installed
#
# The image is built once on first run (~5 min); thereafter reused.

param(
    [string]$Features = "ui-egui-glow"
)

$ErrorActionPreference = "Stop"

$repoRoot = (& git rev-parse --show-toplevel).Trim()
if (-not $repoRoot) { throw "Not inside a git repo." }

# Translate "D:\source\whisper-dictate\..." -> "/mnt/d/source/whisper-dictate/..."
# (Rancher Desktop's WSL distro mounts drives under /mnt/<letter>/.)
function Convert-WindowsPathToWsl([string]$winPath) {
    $normalised = $winPath -replace '\\', '/'
    if ($normalised -match '^([A-Za-z]):/(.*)$') {
        $drive = $Matches[1].ToLower()
        $rest = $Matches[2]
        return "/mnt/$drive/$rest"
    }
    throw "Not a Windows absolute path: $winPath"
}

$wslRepo = Convert-WindowsPathToWsl $repoRoot
Write-Host "[dev-check] repo at $wslRepo (inside rancher-desktop WSL)" -ForegroundColor Cyan

# Always invoke `docker build` -- it's the only way to pick up changes to
# .devcontainer/Dockerfile or .devcontainer/devcontainer.json on an
# already-existing image. Docker's layer cache makes a no-op rebuild
# near-instant (~1-2 s when nothing changed); a stale image is the more
# expensive failure mode (silently divergent toolchain vs CI). Codex
# P2 #414 dev-check.ps1:50 (the previous if-missing guard would diverge
# from CI whenever Dockerfile changed in another worktree).
Write-Host "[dev-check] docker build (Docker cache makes no-op rebuild fast)..." -ForegroundColor Cyan
wsl -d rancher-desktop -- docker build -t whisper-dictate-dev:latest "$wslRepo/.devcontainer/"
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
        '-d', 'rancher-desktop', '--',
        'docker', 'run', '--rm',
        '-v', "${wslRepo}:/repo",
        '-w', '/repo',
        '-e', 'CARGO_HOME=/repo/.cargo-cache',
        '-e', 'RUSTUP_HOME=/repo/.rustup-cache',
        'whisper-dictate-dev:latest'
    ) + $cmd
    & wsl @args
    if ($LASTEXITCODE -ne 0) { throw "container command failed: $($cmd -join ' ')" }
}

Write-Host "[dev-check] cargo fmt --all -- --check" -ForegroundColor Cyan
Invoke-InContainer @(
    'cargo', 'fmt',
    '--manifest-path', 'src/rust/Cargo.toml',
    '--all', '--', '--check'
)

Write-Host "[dev-check] cargo clippy --features $Features -- -D warnings" -ForegroundColor Cyan
Invoke-InContainer @(
    'cargo', 'clippy',
    '--manifest-path', 'src/rust/Cargo.toml',
    '--target-dir', 'target-linux',
    '-p', 'whisper-dictate-app',
    '--all-targets',
    '--features', $Features,
    '--', '-D', 'warnings'
)

# Run the FULL test target set CI runs -- the Ubuntu rust job invokes
# `cargo test ... -p whisper-dictate-app` without `--lib`, which sweeps
# in `src/rust/tests/*` integration targets (cli_worker, runtime_supervisor
# and friends). Restricting locally to `--lib` made dev-check green while
# an integration test was still failing in CI. Codex P2 #414
# dev-check.ps1:77.
Write-Host "[dev-check] cargo test (matches CIs full target set)" -ForegroundColor Cyan
Invoke-InContainer @(
    'cargo', 'test',
    '--manifest-path', 'src/rust/Cargo.toml',
    '--target-dir', 'target-linux',
    '-p', 'whisper-dictate-app'
)

Write-Host "[dev-check] OK -- ready to push" -ForegroundColor Green
