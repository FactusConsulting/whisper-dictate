# =====================================================================
# voice-pi — one-shot, portable setup + launcher (Windows).
#
# Copy the WHOLE voice-pi folder to any Windows machine with an NVIDIA
# GPU, then run this script. It is idempotent:
#   * first run  → finds/installs Python 3.12, builds the venv,
#                   installs deps, downloads the model, launches.
#   * later runs → validates the venv and just launches.
#
# Nothing is hardcoded to a user or path: the code lives next to this
# script ($PSScriptRoot); the venv is machine-local (so copying the
# folder never drags a broken venv along). Different GPU = no change:
# device="cuda" + the nvidia-*-cu12 runtime wheels are card-agnostic;
# all that's required on the new box is an NVIDIA GPU + recent driver.
#
# Run it (PowerShell):  powershell -ExecutionPolicy Bypass -File setup.ps1
# Any args are passed straight to voice_pi.py, e.g.:
#   powershell -ExecutionPolicy Bypass -File setup.ps1 --lang de
# With no args it defaults to:  --paste --model large-v3-turbo
# Stop the running tool by pressing Esc (or Ctrl+C) — frees GPU VRAM.
# =====================================================================
$ErrorActionPreference = 'Stop'
$here   = $PSScriptRoot
$venv   = Join-Path $env:USERPROFILE 'voice-pi-venv'
$venvPy = Join-Path $venv 'Scripts\python.exe'
$req    = Join-Path $here 'requirements-windows.txt'
$app    = Join-Path $here 'voice_pi.py'

# Default launch args if the user passed none.
$runArgs = if ($args.Count -gt 0) { $args } else { @('--paste','--model','large-v3-turbo') }

function Test-MsvcPy312($exe) {
  if (-not (Test-Path $exe)) { return $false }
  $v = & $exe -c "import sys;print('%d.%d'%sys.version_info[:2]);print(sys.version)" 2>$null
  return ($v -and $v[0] -eq '3.12' -and ($v[1] -match '\[MSC'))
}

function Find-Py312 {
  # ONLY the canonical python.org/winget locations — never a broad
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
          (& $venvPy -c "import faster_whisper, numpy, sounddevice, pynput" 2>$null; $LASTEXITCODE -eq 0)

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
Write-Host "Starting voice-pi — press Esc (or Ctrl+C) to stop." -ForegroundColor Cyan
Set-Location $here
& $venvPy $app @runArgs
