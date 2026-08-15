[CmdletBinding()]
param(
    [string]$Version,
    [string]$Root = (Get-Location).Path,
    [switch]$Check,
    [string]$ExpectedVersion
)

$ErrorActionPreference = 'Stop'
$versionPattern = '^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-rc\.[1-9]\d*)?$'
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Read-Utf8File([string]$Path) {
    [System.IO.File]::ReadAllText($Path, $utf8NoBom)
}

function Read-Versions([string]$RepoRoot) {
    $files = @{
        VERSION = Join-Path $RepoRoot 'VERSION'
        CargoToml = Join-Path $RepoRoot 'src/rust/Cargo.toml'
        CargoLock = Join-Path $RepoRoot 'src/rust/Cargo.lock'
        PackageNix = Join-Path $RepoRoot 'nix/package.nix'
    }
    $raw = Read-Utf8File $files.VERSION
    $toml = Read-Utf8File $files.CargoToml
    $lock = Read-Utf8File $files.CargoLock
    $nix = Read-Utf8File $files.PackageNix
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
    if (-not [regex]::IsMatch($distinct[0], $versionPattern)) {
        Write-Error "INVALID source version '$($distinct[0])'; expected x.y.z or x.y.z-rc.N"
        return $false
    }
    Write-Host "OK - all four files agree on $($distinct[0])"
    return $true
}

function Test-ExpectedVersion([string]$RepoRoot, [string]$Expected) {
    if (-not [regex]::IsMatch($Expected, $versionPattern)) {
        Write-Error "INVALID expected release version '$Expected'; expected x.y.z or x.y.z-rc.N"
        return $false
    }
    $sourceVersion = (Read-Versions $RepoRoot).VERSION
    if ($sourceVersion -cne $Expected) {
        Write-Error "RELEASE VERSION MISMATCH: tag version '$Expected' does not match source version '$sourceVersion'. Bump the source version before tagging, or recreate the unpublished tag for '$sourceVersion'."
        return $false
    }
    Write-Host "OK - release tag version $Expected matches the source version"
    return $true
}

$repoRoot = (Resolve-Path -LiteralPath $Root).Path
if ($Check -or [string]::IsNullOrWhiteSpace($Version)) {
    if (-not (Test-Versions $repoRoot)) { exit 1 }
    if (-not [string]::IsNullOrWhiteSpace($ExpectedVersion)) {
        if (-not (Test-ExpectedVersion $repoRoot $ExpectedVersion)) { exit 1 }
    }
    exit 0
}
if (-not [regex]::IsMatch($Version, $versionPattern)) {
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
$lockLineEnding = if ((Get-Content -LiteralPath $paths.CargoLock -Raw).Contains("`r`n")) { "`r`n" } else { "`n" }

function Replace-Required([string]$Text, [string]$Pattern, [string]$Replacement, [string]$Label) {
    $count = [regex]::Matches($Text, $Pattern).Count
    if ($count -ne 1) {
        throw "expected exactly one $Label version block, found $count"
    }
    $updated = [regex]::Replace($Text, $Pattern, $Replacement, 1)
    if ($updated -eq $Text) {
        throw "failed to replace $Label version block"
    }
    return $updated
}

$contents = @{}
$contents[$paths.VERSION] = "$Version`n"
$cargoToml = Read-Utf8File $paths.CargoToml
$cargoLock = Read-Utf8File $paths.CargoLock
$packageNix = Read-Utf8File $paths.PackageNix
$lockBlock = "name = `"whisper-dictate-app`"${lockLineEnding}version = `"$old`""
$lockReplacement = "name = `"whisper-dictate-app`"${lockLineEnding}version = `"$Version`""
$cargoPattern = '(?ms)(^\[package\][^\[]*?^version = )"' + [regex]::Escape($old) + '"'
$cargoReplacement = '${1}"' + $Version + '"'
$contents[$paths.CargoToml] = Replace-Required $cargoToml $cargoPattern $cargoReplacement 'Cargo.toml'
$contents[$paths.CargoLock] = Replace-Required $cargoLock ([regex]::Escape($lockBlock)) $lockReplacement 'Cargo.lock'
$contents[$paths.PackageNix] = Replace-Required $packageNix ([regex]::Escape("version ? `"$old`"")) "version ? `"$Version`"" 'package.nix'
foreach ($path in $contents.Keys) {
    if (-not $contents[$path].Contains($Version)) {
        throw "computed replacement for $path does not contain $Version"
    }
}
# PowerShell 5.1 does not recognise Set-Content's utf8NoBOM encoding name.
# Use .NET directly so the release helper writes the same BOM-free UTF-8 on
# the Windows PowerShell used by local release work and on pwsh in CI.
foreach ($path in $contents.Keys) {
    [System.IO.File]::WriteAllText($path, $contents[$path], $utf8NoBom)
}
"bumped $old -> $Version; verifying:"
if (-not (Test-Versions $repoRoot)) { exit 1 }
