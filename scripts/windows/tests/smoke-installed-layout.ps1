# Extracted release validation; preserve the CI contract.
$ErrorActionPreference = 'Stop'
$setup = $env:SETUP_EXE
if (-not (Test-Path -LiteralPath $setup)) { throw "Setup exe not found at $setup" }

Write-Host "Installing $setup ..."
$p = Start-Process -FilePath $setup `
       -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' `
       -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "Silent install failed with exit code $($p.ExitCode)" }

$appRoot = Join-Path $env:LOCALAPPDATA 'Programs\WhisperDictate'
Write-Host "Install root: $appRoot"
if (-not (Test-Path -LiteralPath $appRoot)) { throw "Install root missing: $appRoot" }

$required = @(
  'wd.exe',
  'wd-gui.exe',
  'benchmark\corpus.json'
)
foreach ($rel in $required) {
  $full = Join-Path $appRoot $rel
  if (-not (Test-Path -LiteralPath $full)) { throw "Missing installed file: $rel" }
  Write-Host "  ok  $rel"
}
foreach ($retired in @('src\python', 'requirements')) {
  $full = Join-Path $appRoot $retired
  if (Test-Path -LiteralPath $full) { throw "Retired payload was installed: $retired" }
  Write-Host "  absent  $retired"
}

$versionFile = Join-Path $appRoot 'VERSION'
if (-not (Test-Path -LiteralPath $versionFile)) { throw "Missing installed VERSION file" }
$installed = (Get-Content -LiteralPath $versionFile -Raw).Trim()
$expected  = $env:VERSION
if ($installed -ne $expected) {
  throw "Installed VERSION '$installed' != expected '$expected'"
}
Write-Host "  ok  VERSION = $installed"
"APP_ROOT=$appRoot" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
