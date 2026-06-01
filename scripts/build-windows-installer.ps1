# Build Windows installers locally without creating a GitHub release.
param(
  [ValidateSet('nvidia', 'cpu', 'amd', 'all')]
  [string]$Variant = 'nvidia',
  [string]$Version = ''
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Version) {
  $desc = ''
  if (Get-Command git -ErrorAction SilentlyContinue) {
    $desc = (& git -C $root describe --tags --always --dirty 2>$null)
  }
  $Version = if ([string]::IsNullOrWhiteSpace($desc)) { '0.0.0.0' } else { $desc.TrimStart('v') }
  $Version = ($Version -replace '[^0-9A-Za-z.-]', '-')
}
if ($Version -notmatch '^\d+\.\d+\.\d+(\.\d+)?$') {
  throw "Inno Setup VersionInfoVersion must be numeric, e.g. 0.2.51.1. Got: $Version"
}

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
cargo build --release -p whisper-dictate-app
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$versionFile = Join-Path $root 'VERSION'
$hadVersion = Test-Path $versionFile
$oldVersion = if ($hadVersion) { Get-Content $versionFile -Raw } else { $null }
Set-Content $versionFile $Version -Encoding ascii
$outDir = Join-Path $root 'Output'
New-Item -ItemType Directory -Force $outDir | Out-Null

try {
  $variants = if ($Variant -eq 'all') { @('cpu', 'nvidia', 'amd') } else { @($Variant) }
  foreach ($v in $variants) {
    Write-Host "Building $v installer version $Version..." -ForegroundColor Cyan
    & $iscc /DVERSION=$Version /DVARIANT=$v /O"$outDir" installer\whisper-dictate.iss
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed for $v" }
  }
} finally {
  if ($hadVersion) {
    Set-Content $versionFile $oldVersion -Encoding ascii
  } else {
    Remove-Item -LiteralPath $versionFile -ErrorAction SilentlyContinue
  }
}

Get-ChildItem $outDir -Filter "whisper-dictate-windows-*-setup-$Version.exe" |
  Select-Object FullName, Length
