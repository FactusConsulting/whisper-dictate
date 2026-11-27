#!/usr/bin/env pwsh
# Validate the Linux shipping profile in the devcontainer image without a
# native Windows MSVC toolchain. PR CI covers alternate feature profiles
# and platform-specific checks.
#
# Usage from any worktree (run from PowerShell with Rancher Desktop running):
#
#   pwsh -NoProfile -File scripts/dev/dev-check.ps1
#   pwsh -NoProfile -File scripts/dev/dev-check.ps1 -DryRun  # print argv; no Docker
#
# Requires:
#   - Rancher Desktop running with its Moby container engine enabled
#
# The image is built once on first run (~5 min); thereafter the layer cache
# keeps subsequent runs near-instant.

[CmdletBinding()]
param([switch]$DryRun)

$ErrorActionPreference = "Stop"

# Git emits UTF-8 on stdout, but Windows PowerShell decodes native output
# with the console codepage, which mangles non-ASCII worktree names into
# paths that no longer match the filesystem and silently skips worktree
# detection below. Decode as UTF-8 when the host allows it; a console-less
# host keeps its default encoding (fine for ASCII checkouts).
try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch { }

$repoRoot = (& git rev-parse --show-toplevel | ForEach-Object { $_.Trim() })
if (-not $repoRoot) { throw "Not inside a git repo." }
# Git emits forward slashes; docker.exe wants the native Windows form
# for its -v mount. Rancher Desktop translates `D:\...` to the Linux
# mount automatically.
$repoRoot = $repoRoot -replace '/', '\'

# On Windows Rancher Desktop serves Docker's standard named pipe through the
# `default` context. Pin it so an installed Docker Desktop context is never
# selected accidentally.
$DockerContext = 'default'
Write-Host "[dev-check] repo at $repoRoot (mounted into devcontainer via Rancher Desktop)" -ForegroundColor Cyan

# Every mount uses `--mount` key=value syntax with a Linux-side source path.
# Rancher Desktop's daemon runs in a WSL VM that sees Windows drives at
# /mnt/<drive>; handing it `D:\path` relies on the Windows-side proxy to
# translate the path, and under load that translation intermittently does
# not happen ("invalid mode: /repo" for `-v`, "mount path must be absolute"
# for `--mount`).
function ConvertTo-DaemonPath([string]$Path) {
    if ($Path -match '^([A-Za-z]):[\\/](.*)$') {
        return "/mnt/$($Matches[1].ToLowerInvariant())/$($Matches[2] -replace '\\', '/')"
    }
    return $Path
}

# `--mount` fields are comma-separated, and Windows PowerShell 5.1 strips the
# CSV quotes docker would need around a value containing a comma, so such a
# path is refused up front instead of being split into bogus mount options.
function New-MountSpec([string]$Type, [string]$Source, [string]$Target) {
    if ($Type -eq 'bind') { $Source = ConvertTo-DaemonPath $Source }
    foreach ($value in @($Source, $Target)) {
        if ($value -match '[,"]') {
            throw "dev-check cannot mount a path containing ',' or '`"': $value -- move the checkout to a path without them"
        }
    }
    return "type=$Type,source=$Source,target=$Target"
}

# A linked worktree's `.git` is a FILE pointing at the main checkout on the
# host. Inside the container only /repo exists, so plain git resolves that
# host path, fails ("not a git repository"), and kills repository_policy's
# `git ls-files` after minutes of green tests. When we detect a linked
# worktree, mount the main `.git` at /main-git and point git at the
# worktree-specific gitdir inside it; a main checkout keeps the plain env.
$GitMountArgs = @()
$GitEnvArgs = @(
    '-e', 'GIT_CONFIG_COUNT=1',
    '-e', 'GIT_CONFIG_KEY_0=safe.directory',
    '-e', 'GIT_CONFIG_VALUE_0=/repo'
)
$dotGit = Join-Path $repoRoot '.git'
# -LiteralPath everywhere: checkout paths may contain wildcard
# metacharacters such as [ ] which -Path would glob.
if (Test-Path -LiteralPath $dotGit -PathType Leaf) {
    # git writes the pointer in UTF-8 (no BOM); the default Get-Content
    # encoding is the ANSI codepage, which mangles non-ASCII worktree names.
    $gitdirLine = (Get-Content -LiteralPath $dotGit -TotalCount 1 -Encoding UTF8).Trim()
    if ($gitdirLine -match '^gitdir:\s*(.+)$') {
        $gitdirPath = $Matches[1].Trim() -replace '\\', '/'
        # Derive the suffix from the real common dir rather than assuming it
        # is named .git: --separate-git-dir checkouts and bare repos point
        # their .git file somewhere else, but the worktree admin dir is
        # always <common-dir>/worktrees/<name>.
        $commonDir = ((git -C $repoRoot rev-parse --git-common-dir).Trim() -replace '\\', '/')
        if (-not (Test-Path -LiteralPath $commonDir -PathType Container)) {
            throw "common git dir not found: $commonDir"
        }
        if (-not $gitdirPath.StartsWith("$commonDir/")) {
            throw "unrecognised gitdir pointer in $dotGit : $gitdirLine"
        }
        $suffix = $gitdirPath.Substring($commonDir.Length + 1)
        if (-not $suffix) {
            throw "empty worktree suffix for $dotGit : $gitdirLine"
        }
        $commonDirMount = $commonDir -replace '/', '\'
        $GitMountArgs = @('--mount', (New-MountSpec 'bind' $commonDirMount '/main-git'))
        $GitEnvArgs = @(
            '-e', ("GIT_DIR=/main-git/$suffix"),
            '-e', 'GIT_WORK_TREE=/repo',
            '-e', 'GIT_CONFIG_COUNT=2',
            '-e', 'GIT_CONFIG_KEY_0=safe.directory',
            '-e', 'GIT_CONFIG_VALUE_0=/repo',
            '-e', 'GIT_CONFIG_KEY_1=safe.directory',
            '-e', 'GIT_CONFIG_VALUE_1=/main-git'
        )
        # Cargo can strip inherited Git variables when launching tests.
        # Persist the repository selection in the CARGO_HOME [env] table
        # for Cargo-spawned processes. Force the assignments so they apply
        # even when Docker already set the same variables. The Docker env
        # also covers nextest and commands run directly in the container.
        $cargoHome = Join-Path $repoRoot '.cargo-cache'
        # .NET API: New-Item lacks -LiteralPath on Windows PowerShell 5.1.
        [System.IO.Directory]::CreateDirectory($cargoHome) | Out-Null
        $cargoEnvConfig = @(
            '[env]',
            ('GIT_DIR = { value = "/main-git/' + $suffix + '", force = true }'),
            'GIT_WORK_TREE = { value = "/repo", force = true }',
            'GIT_CONFIG_COUNT = { value = "2", force = true }',
            'GIT_CONFIG_KEY_0 = { value = "safe.directory", force = true }',
            'GIT_CONFIG_VALUE_0 = { value = "/repo", force = true }',
            'GIT_CONFIG_KEY_1 = { value = "safe.directory", force = true }',
            'GIT_CONFIG_VALUE_1 = { value = "/main-git", force = true }'
        )
        # UTF-8 (no BOM): worktree names can be non-ASCII, and an ascii
        # encoding would mangle them into the config cargo force-applies.
        [System.IO.File]::WriteAllLines((Join-Path $cargoHome 'config.toml'), $cargoEnvConfig, [System.Text.UTF8Encoding]::new($false))
        Write-Host "[dev-check] linked worktree: mounting $commonDir at /main-git (GIT_DIR=/main-git/$suffix; also persisted in .cargo-cache/config.toml [env])" -ForegroundColor Cyan
    }
}

# Use one Cargo feature set and target directory for the local shipping gate.
# The unfiltered nextest run includes repository_policy and builds wd for
# the integration tests, so the CLI smoke can reuse that executable.
$CargoArgs = @(
    '--manifest-path', 'src/rust/Cargo.toml',
    '--locked',
    '--target-dir', 'target-linux',
    '-p', 'whisper-dictate-app'
)
$ShippingCargoArgs = $CargoArgs + @('--no-default-features', '--features', 'shipping')
$legs = @(
    @{
        Name = 'cargo install cargo-nextest --locked'
        Argv = @('cargo', 'install', 'cargo-nextest', '--locked')
    },
    @{
        Name = 'cargo fmt --all -- --check'
        Argv = @('cargo', 'fmt', '--manifest-path', 'src/rust/Cargo.toml', '--all', '--', '--check')
    },
    @{
        Name = 'cargo clippy (ui-egui-glow)'
        Argv = @('cargo', 'clippy') + $CargoArgs + @('--all-targets', '--features', 'ui-egui-glow', '--', '-D', 'warnings')
    },
    @{
        Name = 'cargo nextest run --no-default-features --features shipping'
        Argv = @('cargo', 'nextest', 'run') + $ShippingCargoArgs + @('--profile', 'ci')
    },
    @{
        Name = 'cargo test --doc'
        Argv = @('cargo', 'test') + $ShippingCargoArgs + @('--doc')
    },
    @{
        Name = 'CLI smoke: wd --help'
        Argv = @('./target-linux/debug/wd', '--help')
    },
    @{
        Name = 'CLI smoke: wd --version'
        Argv = @('./target-linux/debug/wd', '--version')
    },
    @{
        Name = 'CLI smoke: wd config path'
        Argv = @('./target-linux/debug/wd', 'config', 'path')
    }
)

# Linux-side build state lives in Docker named volumes, not on the bind
# mount: Rancher Desktop serves the repo over 9p, and the linker's output
# written through that mount intermittently comes back with zeroed pages,
# which surfaces as SIGSEGV in whichever test binary was corrupted. The
# volumes are mounted at the /repo/... paths used by Cargo and the CLI smoke.
#
# The registry and toolchain volumes are shared by every checkout. The
# target volume is per checkout: every worktree is mounted at /repo, so a
# shared target would let cargo's mtime fingerprints accept another
# branch's artefacts for a source file that is older than the last build.
# The name carries a hash of the checkout path (docker volume names are
# ASCII-only; worktree names need not be).
$sha1 = [System.Security.Cryptography.SHA1]::Create()
$rootHash = ($sha1.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($repoRoot.ToLowerInvariant())) |
    ForEach-Object { $_.ToString('x2') }) -join ''
$TargetVolume = "whisper-dictate-dev-target-$($rootHash.Substring(0, 12))"
$RepoMount = New-MountSpec 'bind' $repoRoot '/repo'
$VolumeMounts = @(
    '--mount', (New-MountSpec 'volume' $TargetVolume '/repo/target-linux'),
    '--mount', (New-MountSpec 'volume' 'whisper-dictate-dev-cargo-home' '/repo/.cargo-cache'),
    '--mount', (New-MountSpec 'volume' 'whisper-dictate-dev-rustup-home' '/repo/.rustup-cache')
)
if ($GitMountArgs.Count -gt 0) {
    # The linked-worktree [env] table is written on the host above (and
    # read by the dev-check smoke test there); overlay that single file on
    # the shared cargo-home volume so cargo still picks it up.
    $VolumeMounts += @('--mount', (New-MountSpec 'bind' (Join-Path $repoRoot '.cargo-cache\config.toml') '/repo/.cargo-cache/config.toml'))
}

# A fresh named volume is root-owned while the image runs as `vscode`.
# Chown just the mount points (idempotent, no -R: contents are created by
# vscode afterwards).
$VolumePrepArgs = @('--context', $DockerContext, 'run', '--rm', '--user', 'root') + $VolumeMounts + @(
    'whisper-dictate-dev:latest',
    'chown', 'vscode:vscode', '/repo/target-linux', '/repo/.cargo-cache', '/repo/.rustup-cache'
)
Write-Host "[dev-check] target volume $TargetVolume (registry + toolchain volumes shared)" -ForegroundColor Cyan

# Print the plan and return before any Docker side effects.
if ($DryRun) {
    Write-Host '[dry-run] docker volume prep' -ForegroundColor Yellow
    Write-Host ('  docker ' + ($VolumePrepArgs -join ' '))
    Write-Host '[dry-run] docker volume mounts' -ForegroundColor Yellow
    Write-Host ('  ' + ($VolumeMounts -join ' '))
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
# expensive failure mode (silently divergent toolchain vs CI): an
# if-missing guard would diverge from CI whenever the Dockerfile changes
# in another worktree.
Write-Host "[dev-check] docker build (layer cache makes no-op rebuild fast)..." -ForegroundColor Cyan
docker --context $DockerContext build -t whisper-dictate-dev:latest "$repoRoot\.devcontainer\"
if ($LASTEXITCODE -ne 0) { throw "image build failed" }

& docker @VolumePrepArgs
if ($LASTEXITCODE -ne 0) { throw "volume ownership setup failed" }

function Invoke-InContainer([string[]]$cmd) {
    # Mount the repo at /repo; target-linux / CARGO_HOME / RUSTUP_HOME are
    # the named volumes above, so the pinned toolchain survives across
    # `docker run --rm` invocations without being downloaded per call
    # (`docker run --rm` does NOT run the devcontainer `postCreateCommand`).
    #
    # Pass the command as an argument array directly to Docker, without
    # shell interpolation.
    $args = @(
        '--context', $DockerContext,
        'run', '--rm',
        '--mount', $RepoMount
        # Empty for a main checkout; mounts the common git dir at /main-git
        # for a linked worktree so git can resolve the worktree gitdir.
    ) + $GitMountArgs + $VolumeMounts + @(
        '-w', '/repo',
        '-e', 'CARGO_HOME=/repo/.cargo-cache',
        '-e', 'RUSTUP_HOME=/repo/.rustup-cache'
        # The Windows bind mount has a different owner inside Linux.  Let
        # Git inspect this explicitly mounted checkout during policy tests.
        # Linked-worktree mode swaps this for a COUNT=2 variant pointing
        # git at the /main-git mount above.
    ) + $GitEnvArgs + @(
        'whisper-dictate-dev:latest'
    ) + $cmd
    & docker @args
    if ($LASTEXITCODE -ne 0) { throw "container command failed: $($cmd -join ' ')" }
}

foreach ($leg in $legs) {
    Write-Host "[dev-check] $($leg.Name)" -ForegroundColor Cyan
    Write-Host ('  ' + ($leg.Argv -join ' '))
    Invoke-InContainer $leg.Argv
}

Write-Host "[dev-check] OK -- ready to push" -ForegroundColor Green
