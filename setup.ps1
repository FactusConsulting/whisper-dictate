# =====================================================================
# whisper-dictate - one-shot, portable setup + launcher (Windows).
#
# Copy the WHOLE folder to any Windows machine and run this. Idempotent:
#   * first run  -> finds/installs Python 3.12, builds the venv,
#                   installs deps, downloads the model, launches.
#   * later runs -> validates the venv and just launches.
#
# Nothing is hardcoded to a user or path: code lives next to this
# script ($PSScriptRoot); the venv is machine-local. Requirements:
# the GPU bundle ships requirements-gpu.txt (NVIDIA/CUDA); the CPU
# bundle ships requirements-cpu.txt (no GPU, incl. AMD-GPU boxes). A
# release bundle ships exactly one as requirements.txt - preferred if
# present, else this falls back to the GPU file in a dev checkout.
# voice_pi.py auto-detects CUDA vs CPU at runtime (see --device).
#
# Run it (PowerShell):  powershell -ExecutionPolicy Bypass -File setup.ps1
# Any args pass straight to voice_pi.py, e.g. ... -File setup.ps1 --lang de
# With no args it uses voice_pi.py defaults (auto injection strategy,
# fastest model large-v3-turbo).
# Stop the running tool by pressing Esc (or Ctrl+C) - frees GPU VRAM.
# =====================================================================
$ErrorActionPreference = 'Stop'
try {
  [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()
  $OutputEncoding = [System.Text.UTF8Encoding]::new()
} catch {
}
$here   = $PSScriptRoot
$venv   = Join-Path $env:USERPROFILE 'voice-pi-venv'
$venvPy = Join-Path $venv 'Scripts\python.exe'
$app    = Join-Path $here 'voice_pi.py'
$reqStamp = Join-Path $venv '.requirements.sha256'
$versionFile = Join-Path $here 'VERSION'
$version = if (Test-Path $versionFile) {
    (Get-Content $versionFile -TotalCount 1).Trim()
} elseif (Get-Command git -ErrorAction SilentlyContinue) {
    $gitVersion = (& git -C $here describe --tags --always --dirty 2>$null)
    if ([string]::IsNullOrWhiteSpace($gitVersion)) { 'dev' } else { $gitVersion.TrimStart('v') }
} else {
    'dev'
}
Write-Host "whisper-dictate $version"
$env:VOICEPI_LAUNCHER_PRINTED_VERSION = '1'

[string[]]$runArgs = if ($args.Count -gt 0) {
    $args
} else {
    @()
}

function Test-WantsCuda([string[]]$argv) {
  $envDevice = if ($env:VOICEPI_DEVICE) { $env:VOICEPI_DEVICE.ToLowerInvariant() } else { '' }
  if ($envDevice -eq 'cuda') { return $true }
  $configDevice = Get-VoicePiConfigValue 'device'
  if ($configDevice -and $configDevice.ToLowerInvariant() -eq 'cuda') { return $true }
  for ($i = 0; $i -lt $argv.Count; $i++) {
    if ($argv[$i] -eq '--device' -and ($i + 1) -lt $argv.Count -and $argv[$i + 1] -eq 'cuda') { return $true }
    if ($argv[$i] -eq '--device=cuda') { return $true }
  }
  return $false
}

function Test-NvidiaPresent {
  if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) { return $true }
  try {
    $gpus = Get-CimInstance Win32_VideoController -ErrorAction Stop
    return [bool]($gpus | Where-Object { $_.Name -match 'NVIDIA' } | Select-Object -First 1)
  } catch {
    return $false
  }
}

function Select-Requirements([string[]]$argv) {
  $bundleReq = Join-Path $here 'requirements.txt'
  $gpuReq = Join-Path $here 'requirements-gpu.txt'
  $cpuReq = Join-Path $here 'requirements-cpu.txt'
  if (Test-Path $bundleReq) { return $bundleReq }
  if (Test-WantsCuda $argv) {
    if (Test-Path $gpuReq) { return $gpuReq }
    throw "--device cuda requested, but requirements-gpu.txt is missing"
  }
  if ((Test-NvidiaPresent) -and (Test-Path $gpuReq)) { return $gpuReq }
  if (Test-Path $cpuReq) { return $cpuReq }
  if (Test-Path $gpuReq) { return $gpuReq }
  throw "no requirements file found next to setup.ps1"
}

function Get-VoicePiConfigPath {
  if ($env:VOICEPI_CONFIG) { return [Environment]::ExpandEnvironmentVariables($env:VOICEPI_CONFIG) }
  $base = if ($env:APPDATA) { $env:APPDATA } else { Join-Path $env:USERPROFILE 'AppData\Roaming' }
  return (Join-Path $base 'WhisperDictate\config.json')
}

function Get-VoicePiConfigValue([string]$key) {
  $cfg = Get-VoicePiConfigPath
  if (-not (Test-Path $cfg)) { return $null }
  try {
    $data = Get-Content $cfg -Raw | ConvertFrom-Json
    $prop = $data.PSObject.Properties[$key]
    if ($prop -and -not [string]::IsNullOrWhiteSpace([string]$prop.Value)) {
      return [string]$prop.Value
    }
  } catch {
    Write-Warning "Could not read config ${cfg}: $_"
  }
  return $null
}

function Get-VoicePiFileHash([string]$path) {
  if (Get-Command Get-FileHash -ErrorAction SilentlyContinue) {
    return (Get-FileHash -Algorithm SHA256 $path).Hash
  }
  $sha = [System.Security.Cryptography.SHA256]::Create()
  $stream = [System.IO.File]::OpenRead($path)
  try {
    $bytes = $sha.ComputeHash($stream)
    return ([System.BitConverter]::ToString($bytes)).Replace('-', '')
  } finally {
    $stream.Dispose()
    $sha.Dispose()
  }
}

function Test-WantsParakeet {
  $configuredBackend = Get-VoicePiConfigValue 'stt_backend'
  if ($configuredBackend) { return ($configuredBackend.ToLowerInvariant() -eq 'parakeet') }
  if ($env:VOICEPI_STT_BACKEND) { return ($env:VOICEPI_STT_BACKEND.ToLowerInvariant() -eq 'parakeet') }
  return $false
}

function Test-LaunchesDictation([string[]]$argv) {
  foreach ($arg in $argv) {
    if ($arg -in @(
      '--settings-ui',
      '--doctor',
      '--dictionary-status',
      '--dictionary-open',
      '--dictionary-add',
      '--dictionary-replace',
      '--help',
      '-h'
    )) {
      return $false
    }
  }
  return $true
}

function Test-ParakeetReady {
  if (-not (Test-Path $venvPy)) { return $false }
  & $venvPy -c "import importlib.util, sys; sys.exit(0 if importlib.util.find_spec('nemo.collections.asr') else 1)" *> $null
  return ($LASTEXITCODE -eq 0)
}

function Test-TorchCudaReady {
  if (-not (Test-Path $venvPy)) { return $false }
  & $venvPy -c "import sys, torch; sys.exit(0 if torch.cuda.is_available() else 1)" *> $null
  return ($LASTEXITCODE -eq 0)
}

$req = Select-Requirements $runArgs
$reqHash = Get-VoicePiFileHash $req
$wantsParakeet = (Test-LaunchesDictation $runArgs) -and (Test-WantsParakeet)
$wantsParakeetCuda = $wantsParakeet -and ((Test-WantsCuda $runArgs) -or ((Get-VoicePiConfigValue 'device') -in @($null, '', 'auto') -and (Test-NvidiaPresent)))
$parakeetReq = Join-Path $here 'requirements-parakeet.txt'
$parakeetStamp = Join-Path $venv '.requirements-parakeet.sha256'
$pipInstallArgs = @("--disable-pip-version-check", "--progress-bar", "off")
$torchCudaIndex = "https://download.pytorch.org/whl/cu121"

function Test-MsvcPy312($exe) {
  if (-not (Test-Path $exe)) { return $false }
  $v = & $exe -c "import sys;print('%d.%d'%sys.version_info[:2]);print(sys.version)" 2>$null
  return ($v -and $v[0] -eq '3.12' -and ($v[1] -match '\[MSC'))
}

function Find-Py312 {
  # ONLY the canonical python.org/winget locations - never a broad
  # Program Files recurse (that picks up Autodesk/Blender/etc.).
  $cands = @(
    "$env:LOCALAPPDATA\Programs\Python\Python312\python.exe",
    "$env:ProgramFiles\Python312\python.exe"
  )
  foreach ($p in $cands) { if (Test-MsvcPy312 $p) { return $p } }
  return $null
}

# --- 1. fast path: a valid venv that can already import the engine ---
$storedReqHash = if (Test-Path $reqStamp) { (Get-Content $reqStamp -Raw).Trim() } else { '' }
$venvOk = (Test-MsvcPy312 $venvPy) -and
          ($storedReqHash -eq $reqHash) -and
          $(& $venvPy -c "import faster_whisper, numpy, sounddevice, pynput" 2>$null; $LASTEXITCODE -eq 0)

if (-not $venvOk) {
  Write-Host "Setting up whisper-dictate (one-time on this machine)..." -ForegroundColor Cyan
  Write-Host "Requirements: $([System.IO.Path]::GetFileName($req))" -ForegroundColor Cyan

  # --- 2. ensure an official MSVC CPython 3.12 ---
  $py = Find-Py312
  if (-not $py) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
      throw "Python 3.12 not found and winget unavailable. Install 64-bit Python 3.12 from https://www.python.org/downloads/ ('Just me'), then re-run."
    }
    Write-Host "Installing official Python 3.12 (user scope)..." -ForegroundColor Yellow
    winget install -e --id Python.Python.3.12 --scope user --silent `
      --accept-package-agreements --accept-source-agreements
    Start-Sleep -Seconds 3
    $py = Find-Py312
  }
  if (-not $py) { throw "Python 3.12 still not found after install attempt." }
  Write-Host "Python 3.12: $py" -ForegroundColor Green

  # --- 3. (re)build the venv from that interpreter ---
  if (Test-Path $venv) { Remove-Item -Recurse -Force $venv }
  & $py -m venv $venv
  if ($LASTEXITCODE -ne 0) { throw "venv creation failed" }
  & $venvPy -m pip install @pipInstallArgs --upgrade pip
  if ($LASTEXITCODE -ne 0) { throw "pip upgrade failed" }
  & $venvPy -m pip install @pipInstallArgs -r $req
  if ($LASTEXITCODE -ne 0) { throw "dependency install failed (see error above)" }
  Set-Content -Path $reqStamp -Value $reqHash -Encoding ASCII
  Write-Host "Setup complete." -ForegroundColor Green
}

if ($wantsParakeet) {
  if (-not (Test-Path $parakeetReq)) {
    throw "VOICEPI_STT_BACKEND=parakeet is configured, but requirements-parakeet.txt is missing next to setup.ps1"
  }
  if ($wantsParakeetCuda -and -not (Test-TorchCudaReady)) {
    Write-Host "Installing CUDA PyTorch for NVIDIA Parakeet..." -ForegroundColor Cyan
    & $venvPy -m pip install @pipInstallArgs --upgrade --force-reinstall torch torchaudio --index-url $torchCudaIndex
    if ($LASTEXITCODE -ne 0) { throw "CUDA PyTorch install failed (see error above)" }
  }
  $parakeetHash = Get-VoicePiFileHash $parakeetReq
  $storedParakeetHash = if (Test-Path $parakeetStamp) { (Get-Content $parakeetStamp -Raw).Trim() } else { '' }
  if (($storedParakeetHash -ne $parakeetHash) -or -not (Test-ParakeetReady)) {
    Write-Host "Installing optional NVIDIA Parakeet dependencies..." -ForegroundColor Cyan
    & $venvPy -m pip install @pipInstallArgs -r $parakeetReq
    if ($LASTEXITCODE -ne 0) { throw "Parakeet dependency install failed (see error above)" }
    Set-Content -Path $parakeetStamp -Value $parakeetHash -Encoding ASCII
  }
}

# --- 4. launch (first run also downloads the model, ~1.5-3 GB once) ---
Write-Host "Starting whisper-dictate - press Esc (or Ctrl+C) to stop." -ForegroundColor Cyan
Set-Location $here
& $venvPy $app $runArgs
exit $LASTEXITCODE
