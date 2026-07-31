[CmdletBinding()]
param(
    [string]$Version,
    [string]$Root = (Get-Location).Path,
    [switch]$Check
)

$ErrorActionPreference = 'Stop'
$versionPattern = '^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-rc\.[1-9]\d*)?$'

function Read-Versions([string]$RepoRoot) {
    $files = @{
        VERSION = Join-Path $RepoRoot 'VERSION'
        CargoToml = Join-Path $RepoRoot 'src/rust/Cargo.toml'
        CargoLock = Join-Path $RepoRoot 'src/rust/Cargo.lock'
        PackageNix = Join-Path $RepoRoot 'nix/package.nix'
    }
    $raw = Get-Content -LiteralPath $files.VERSION -Raw
    $toml = Get-Content -LiteralPath $files.CargoToml -Raw
    $lock = Get-Content -LiteralPath $files.CargoLock -Raw
    $nix = Get-Content -LiteralPath $files.PackageNix -Raw
    [ordered]@{
        VERSION = $raw.Trim()
        'Cargo.toml' = ([regex]::Match($toml, '(?m)^version = "([^"]+)"\r?$')).Groups[1].Value
        'Cargo.lock' = ([regex]::Match($lock, 'name = "whisper-dictate-app"\r?\nversion = "([^"]+)"')).Groups[1].Value
        'package.nix' = ([regex]::Match($nix, 'version \? "([^"]+)"')).Groups[1].Value
    }
}

function Test-Versions([string]$RepoRoot) {
    $versions = Read-Versions $RepoRoot
    $versions.GetEnumerator() | ForEach-Object { Write-Host "  $($_.Key): $($_.Value)" }
    $distinct = @($versions.Values | Select-Object -Unique)
    if ($distinct.Count -ne 1 -or [string]::IsNullOrWhiteSpace($distinct[0])) {
        Write-Error 'INCONSISTENT version files'
        return $false
    }
    Write-Host "OK - all four files agree on $($distinct[0])"
    return $true
}

$repoRoot = (Resolve-Path -LiteralPath $Root).Path
if ($Check -or [string]::IsNullOrWhiteSpace($Version)) {
    if (-not (Test-Versions $repoRoot)) { exit 1 }
    exit 0
}
if ($Version -notmatch $versionPattern) {
    Write-Error "not a x.y.z or x.y.z-rc.N version: '$Version'"
    exit 1
}
if (-not (Test-Versions $repoRoot)) { exit 1 }
$old = (Read-Versions $repoRoot).VERSION
if ($old -eq $Version) { "already at $Version"; exit 0 }

$paths = @{
    VERSION = Join-Path $repoRoot 'VERSION'
    CargoToml = Join-Path $repoRoot 'src/rust/Cargo.toml'
    CargoLock = Join-Path $repoRoot 'src/rust/Cargo.lock'
    PackageNix = Join-Path $repoRoot 'nix/package.nix'
}
$contents = @{}
$contents[$paths.VERSION] = "$Version`n"
$contents[$paths.CargoToml] = [regex]::Replace((Get-Content $paths.CargoToml -Raw), [regex]::Escape("version = `"$old`""), "version = `"$Version`"", 1)
$contents[$paths.CargoLock] = [regex]::Replace((Get-Content $paths.CargoLock -Raw), [regex]::Escape("name = `"whisper-dictate-app`"`nversion = `"$old`""), "name = `"whisper-dictate-app`"`nversion = `"$Version`"", 1)
$contents[$paths.PackageNix] = [regex]::Replace((Get-Content $paths.PackageNix -Raw), [regex]::Escape("version ? `"$old`""), "version ? `"$Version`"", 1)
foreach ($path in $contents.Keys) {
    Set-Content -LiteralPath $path -Value $contents[$path] -Encoding utf8NoBOM -NoNewline
}
"bumped $old -> $Version; verifying:"
if (-not (Test-Versions $repoRoot)) { exit 1 }
