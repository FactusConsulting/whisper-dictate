# Extracted release validation; preserve the CI contract.
$ErrorActionPreference = 'Stop'
$exe = Join-Path $env:APP_ROOT 'wd-gui.exe'
if (-not (Test-Path $exe)) { throw "GUI binary missing at $exe" }

$mesaVersion = '26.1.5'
$mesaUrl = 'https://github.com/mmozeiko/build-mesa/releases/download/26.1.5/mesa-llvmpipe-x64-26.1.5.7z'
$mesaArchive = Join-Path $env:RUNNER_TEMP "mesa-llvmpipe-x64-$mesaVersion.7z"
$mesaDir = Join-Path $env:RUNNER_TEMP "mesa-llvmpipe-x64-$mesaVersion"
$expectedSha256 = '1f691c0c8bf386c05c294b8b799fb110e84890400aa3441b013790cb5f503033'

& curl.exe --fail --location --retry 3 --output $mesaArchive $mesaUrl
if ($LASTEXITCODE -ne 0) { throw "Mesa download failed with exit code $LASTEXITCODE" }
$actualSha256 = (Get-FileHash -LiteralPath $mesaArchive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualSha256 -ne $expectedSha256) {
  throw "Mesa archive SHA-256 mismatch: expected $expectedSha256, got $actualSha256"
}
New-Item -ItemType Directory -Path $mesaDir -Force | Out-Null
& 7z.exe x $mesaArchive "-o$mesaDir" -y
if ($LASTEXITCODE -ne 0) { throw "Mesa extraction failed with exit code $LASTEXITCODE" }
$mesaOpenGl = Join-Path $mesaDir 'opengl32.dll'
if (-not (Test-Path -LiteralPath $mesaOpenGl)) {
  throw "Mesa opengl32.dll missing after extraction"
}
Copy-Item -LiteralPath $mesaOpenGl -Destination (Join-Path $env:APP_ROOT 'opengl32.dll') -Force
$env:GALLIUM_DRIVER = 'llvmpipe'

$errLog = "gui-smoke.err"
$outLog = "gui-smoke.out"
Write-Host "Launching $exe (no args) with Mesa llvmpipe for ~10 s..."
$p = Start-Process -FilePath $exe `
       -RedirectStandardError $errLog -RedirectStandardOutput $outLog `
       -PassThru -WindowStyle Hidden
$procId = $p.Id
$survived = $false
for ($i = 1; $i -le 10; $i++) {
  Start-Sleep -Seconds 1
  if ($p.HasExited) {
    Write-Host "GUI binary exited early after ${i}s with code $($p.ExitCode)"
    break
  }
  if ($i -eq 10) {
    Write-Host "GUI binary still running after 10 s — startup OK; killing."
    $survived = $true
    Stop-Process -Id $procId -Force
    $p.WaitForExit(5000) | Out-Null
  }
}
Write-Host "===== gui-smoke stderr ====="
if (Test-Path $errLog) { Get-Content $errLog } else { Write-Host "(no stderr)" }
Write-Host "===== gui-smoke stdout ====="
if (Test-Path $outLog) { Get-Content $outLog } else { Write-Host "(no stdout)" }
Write-Host "============================"
if ($survived) {
  Write-Host "  ok  wd-gui.exe survived startup"
} else {
  $exit = $p.ExitCode
  throw "wd-gui.exe exited early (code $exit) - the tray must stay alive"
}
