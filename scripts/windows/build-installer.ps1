# Build Windows installers locally without creating a GitHub release.
param(
  [string]$Version = ''
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $root

function Get-CrateVersion {
  $cargoToml = Join-Path $root 'src\rust\Cargo.toml'
  $match = Select-String -LiteralPath $cargoToml -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
  if (-not $match) {
    throw "Could not read package version from $cargoToml"
  }
  return $match.Matches[0].Groups[1].Value
}

function Get-LocalBuildMetadata {
  $stamp = Get-Date -Format 'yyyyMMddHHmmss'
  $sha = ''
  $dirty = ''
  if (Get-Command git -ErrorAction SilentlyContinue) {
    $sha = (& git -C $root rev-parse --short HEAD 2>$null)
    if (-not [string]::IsNullOrWhiteSpace($sha)) {
      $sha = ".g$($sha.Trim())"
    }
    $status = (& git -C $root status --porcelain 2>$null)
    if ($status) {
      $dirty = ".dirty"
    }
  }
  return "local.$stamp$sha$dirty"
}

function Get-VersionInfoVersion([string]$DisplayVersion) {
  # Strip a semver prerelease suffix (e.g. `-rc.1`) before pattern-matching,
  # mirroring what `.github/workflows/windows-installer-build.yml` does for
  # VERSION_INFO. Inno Setup's VersionInfoVersion is a Windows
  # `FILEVERSION`/`PRODUCTVERSION` field that only accepts numeric dotted
  # forms; the prerelease tag is fine in AppVersion / the output filename
  # but breaks the resource compile. Without this strip the local Windows
  # installer loop blows up during the RC window (P2 #406 Codex finding).
  #
  # The lookahead `(?=$|\+)` matches `-rc.N` whether it terminates the
  # version (`1.19.0-rc.1`) or precedes local build metadata
  # (`1.19.0-rc.1+local.20260603073512.gabc1234` — the default path when
  # `-Version` is not passed during the RC window).
  $numeric = $DisplayVersion -replace '-rc\.\d+(?=$|\+)',''
  if ($numeric -match '^(\d+\.\d+\.\d+)\.(\d+)$') {
    return $numeric
  }
  if ($numeric -match '^(\d+\.\d+\.\d+)\+') {
    return "$($Matches[1]).1"
  }
  if ($numeric -match '^(\d+\.\d+\.\d+)$') {
    return $numeric
  }
  throw "Version must be numeric, semver build metadata, or semver -rc.N, e.g. 0.3.25, 0.3.25.1, 0.3.25+local.20260603073512.gabc1234, or 1.19.0-rc.1. Got: $DisplayVersion"
}

if (-not $Version) {
  $Version = "$(Get-CrateVersion)+$(Get-LocalBuildMetadata)"
}
$versionInfo = Get-VersionInfoVersion $Version

function Find-Iscc {
  $candidates = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
  )
  foreach ($path in $candidates) {
    if ($path -and (Test-Path $path)) { return $path }
  }
  $cmd = Get-Command iscc.exe -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  return $null
}

$iscc = Find-Iscc
if (-not $iscc) {
  if (Get-Command winget -ErrorAction SilentlyContinue) {
    Write-Host "Installing Inno Setup 6 via winget..." -ForegroundColor Cyan
    winget install -e --id JRSoftware.InnoSetup --scope user --silent `
      --accept-package-agreements --accept-source-agreements
    $iscc = Find-Iscc
  }
}
if (-not $iscc) {
  if (Get-Command choco -ErrorAction SilentlyContinue) {
    Write-Host "Installing Inno Setup 6 via Chocolatey..." -ForegroundColor Cyan
    choco install innosetup -y --no-progress
    $iscc = Find-Iscc
  }
}
if (-not $iscc) {
  throw "Inno Setup compiler ISCC.exe was not found. Install Inno Setup 6, then rerun this script."
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw "cargo was not found. Install Rust, then rerun this script."
}
Write-Host "Building Rust desktop UI..." -ForegroundColor Cyan
# Keep this --features list in lockstep with .github/workflows/windows-installer-build.yml
# so local installer builds match the artifact CI ships. P2 #400 Codex finding 4.
cargo build --manifest-path (Join-Path $root 'src\rust\Cargo.toml') --target-dir (Join-Path $root 'target') --release -p whisper-dictate-app --features rust-injection,rust-hotkeys
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$versionFile = Join-Path $root 'VERSION'
$hadVersion = Test-Path $versionFile
$oldVersion = if ($hadVersion) { Get-Content $versionFile -Raw } else { $null }
Set-Content $versionFile $Version -Encoding ascii -NoNewline
$outDir = Join-Path $root 'Output'
New-Item -ItemType Directory -Force $outDir | Out-Null

try {
  Write-Host "Building unified Windows installer version $Version (file version $versionInfo)..." -ForegroundColor Cyan
  & $iscc /DVERSION=$Version /DVERSION_INFO=$versionInfo /O"$outDir" packaging\windows\inno\whisper-dictate.iss
  if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

  Write-Host "Building unified Windows portable ZIP version $Version..." -ForegroundColor Cyan
  $zipRoot = Join-Path $outDir "zip-windows"
  $bundle = Join-Path $zipRoot 'whisper-dictate'
  Remove-Item -LiteralPath $zipRoot -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force $bundle | Out-Null
  # Ship only the Python worker package — not src\rust (compiled into the exe)
  # or the test trees. Mirrors the Inno installer's [Files] list.
  $bundlePython = Join-Path $bundle 'src\python'
  New-Item -ItemType Directory -Force $bundlePython | Out-Null
  Copy-Item -LiteralPath (Join-Path $root 'src\python\whisper_dictate') -Destination $bundlePython -Recurse
  Get-ChildItem -LiteralPath $bundlePython -Directory -Recurse -Force -Filter '__pycache__' |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
  Copy-Item -LiteralPath (Join-Path $root 'README.md'), (Join-Path $root 'LICENSE'), $versionFile -Destination $bundle
  Copy-Item -LiteralPath (Join-Path $root 'docs') -Destination $bundle -Recurse
  Copy-Item -LiteralPath (Join-Path $root 'requirements') -Destination $bundle -Recurse
  $assetDir = Join-Path $bundle 'assets'
  New-Item -ItemType Directory -Force $assetDir | Out-Null
  Copy-Item -LiteralPath (Join-Path $root 'assets\whisper-dictate.ico') -Destination $assetDir
  Copy-Item -LiteralPath (Join-Path $root 'target\release\whisper-dictate.exe') -Destination $bundle
  # Ship the golden-benchmark manifest (corpus.json only — NOT the user-local,
  # gitignored audio) so "Run benchmark" resolves a corpus out of the box.
  $benchmarkDir = Join-Path $bundle 'benchmark'
  New-Item -ItemType Directory -Force $benchmarkDir | Out-Null
  Copy-Item -LiteralPath (Join-Path $root 'benchmark\corpus.json') -Destination $benchmarkDir
  $scriptDir = Join-Path $bundle 'scripts'
  New-Item -ItemType Directory -Force $scriptDir | Out-Null
  Copy-Item -LiteralPath (Join-Path $root 'scripts\dev\inject-smoke.py') -Destination $scriptDir
  $zipPath = Join-Path $outDir "whisper-dictate-windows-$Version.zip"
  Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
  Compress-Archive -Path $bundle -DestinationPath $zipPath -CompressionLevel Optimal
} finally {
  if ($hadVersion) {
    Set-Content $versionFile $oldVersion.TrimEnd("`r", "`n") -Encoding ascii -NoNewline
  } else {
    Remove-Item -LiteralPath $versionFile -ErrorAction SilentlyContinue
  }
}

Get-ChildItem $outDir -Filter "whisper-dictate-windows-setup-$Version.exe" |
  Select-Object FullName, Length
Get-ChildItem $outDir -Filter "whisper-dictate-windows-$Version.zip" |
  Select-Object FullName, Length
