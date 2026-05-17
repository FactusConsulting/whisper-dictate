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
# With no args it defaults to:  --paste   (model defaults to the
# fastest, large-v3-turbo, in voice_pi.py).
# Stop the running tool by pressing Esc (or Ctrl+C) - frees GPU VRAM.
# =====================================================================
$ErrorActionPreference = 'Stop'
$here   = $PSScriptRoot
$venv   = Join-Path $env:USERPROFILE 'voice-pi-venv'
$venvPy = Join-Path $venv 'Scripts\python.exe'
$app    = Join-Path $here 'voice_pi.py'

# Requirements: bundle's requirements.txt wins; else GPU file (dev
# checkout default on Windows); else the CPU file.
$req = @(
  (Join-Path $here 'requirements.txt'),
  (Join-Path $here 'requirements-gpu.txt'),
  (Join-Path $here 'requirements-cpu.txt')
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $req) { throw "no requirements file found next to setup.ps1" }

# Default launch args if the user passed none (turbo is the model
# default inside voice_pi.py, so --paste is enough).
[string[]]$runArgs = if ($args.Count -gt 0) { $args } else { @('--paste') }

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
$venvOk = (Test-MsvcPy312 $venvPy) -and
          $(& $venvPy -c "import faster_whisper, numpy, sounddevice, pynput" 2>$null; $LASTEXITCODE -eq 0)

if (-not $venvOk) {
  Write-Host "Setting up voice-pi (one-time on this machine)..." -ForegroundColor Cyan

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
  & $venvPy -m pip install --upgrade pip
  if ($LASTEXITCODE -ne 0) { throw "pip upgrade failed" }
  & $venvPy -m pip install -r $req
  if ($LASTEXITCODE -ne 0) { throw "dependency install failed (see error above)" }
  Write-Host "Setup complete." -ForegroundColor Green
}

# --- 4. launch (first run also downloads the model, ~1.5-3 GB once) ---
Write-Host "Starting voice-pi - press Esc (or Ctrl+C) to stop." -ForegroundColor Cyan
Set-Location $here
& $venvPy $app $runArgs
