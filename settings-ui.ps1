# Hidden launcher for the optional PySide/Qt settings UI.
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$venvPy = Join-Path $env:USERPROFILE 'voice-pi-venv\Scripts\python.exe'
$app = Join-Path $here 'voice_pi.py'
$uiReq = Join-Path $here 'requirements-ui.txt'
$logDir = Join-Path $env:APPDATA 'WhisperDictate'
$log = Join-Path $logDir 'settings-ui.log'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
if (Get-Variable PSNativeCommandUseErrorActionPreference -Scope Global -ErrorAction SilentlyContinue) {
  $global:PSNativeCommandUseErrorActionPreference = $false
}

function Show-LaunchError([string]$message) {
  try {
    $wshell = New-Object -ComObject WScript.Shell
    $null = $wshell.Popup($message, 0, 'whisper-dictate', 0x10)
  } catch {
    Write-Host $message
  }
}

try {
  "[$(Get-Date -Format o)] starting settings UI launcher" | Out-File -FilePath $log -Append -Encoding utf8
  $env:PIP_PROGRESS_BAR = 'off'
  if (-not (Test-Path $venvPy)) {
    "[$(Get-Date -Format o)] venv missing; running setup.ps1 --settings-ui" | Out-File -FilePath $log -Append -Encoding utf8
    powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File (Join-Path $here 'setup.ps1') --settings-ui *>> $log
    exit $LASTEXITCODE
  }
  & $venvPy -c "import PySide6" *> $null
  if ($LASTEXITCODE -ne 0 -and (Test-Path $uiReq)) {
    "[$(Get-Date -Format o)] installing UI dependencies" | Out-File -FilePath $log -Append -Encoding utf8
    & $venvPy -m pip install --disable-pip-version-check --progress-bar off -r $uiReq *>> $log
  }
  $oldEap = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try {
    & $venvPy $app --settings-ui *>> $log
  } finally {
    $ErrorActionPreference = $oldEap
  }
} catch {
  $msg = "Could not start whisper-dictate Settings UI. See log: $log`n`n$_"
  $msg | Out-File -FilePath $log -Append -Encoding utf8
  Show-LaunchError $msg
}
