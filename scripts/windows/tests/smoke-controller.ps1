# Extracted release validation; preserve the CI contract.
$ErrorActionPreference = 'Stop'
$exe = Join-Path $env:APP_ROOT 'wd.exe'
Write-Host "Running: wd.exe --version"
$p = Start-Process -FilePath $exe -ArgumentList '--version' -Wait -PassThru -WindowStyle Hidden
if ($p.ExitCode -ne 0) { throw "wd.exe --version exited $($p.ExitCode)" }
Write-Host "  ok  --version exit 0"

Write-Host "Running: wd.exe config path"
$p2 = Start-Process -FilePath $exe -ArgumentList 'config','path' -Wait -PassThru -WindowStyle Hidden
if ($p2.ExitCode -ne 0) { throw "wd.exe config path exited $($p2.ExitCode)" }
Write-Host "  ok  config path exit 0"

$doctorOut = Join-Path $env:RUNNER_TEMP 'doctor.json'
$doctorErr = Join-Path $env:RUNNER_TEMP 'doctor.err'
$p3 = Start-Process -FilePath $exe -ArgumentList 'doctor','--json' `
        -RedirectStandardOutput $doctorOut -RedirectStandardError $doctorErr `
        -Wait -PassThru -WindowStyle Hidden
if ($p3.ExitCode -notin @(0, 1)) {
  throw "wd.exe doctor --json exited $($p3.ExitCode)"
}
$null = Get-Content -LiteralPath $doctorOut -Raw | ConvertFrom-Json
Write-Host "  ok  native doctor produced JSON (exit $($p3.ExitCode))"
