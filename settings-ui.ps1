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

function Stop-OldWhisperDictateProcesses {
  try {
    $needle = $here
    $old = Get-CimInstance Win32_Process | Where-Object {
      $_.ProcessId -ne $PID -and
      $_.CommandLine -and
      $_.CommandLine.Contains($needle) -and
      ($_.CommandLine -match 'voice_pi\.py|settings-ui\.ps1|setup\.ps1|setup\.cmd')
    }
    foreach ($proc in $old) {
      "[$(Get-Date -Format o)] stopping old whisper-dictate process $($proc.ProcessId): $($proc.Name)" |
        Out-File -FilePath $log -Append -Encoding utf8
      try {
        & taskkill.exe /PID $proc.ProcessId /T /F *> $null
      } catch {
        Stop-Process -Id $proc.ProcessId -Force -ErrorAction SilentlyContinue
      }
    }
  } catch {
    "[$(Get-Date -Format o)] old process cleanup failed: $_" |
      Out-File -FilePath $log -Append -Encoding utf8
  }
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
  Stop-OldWhisperDictateProcesses
  $env:PIP_PROGRESS_BAR = 'raw'
  if (-not (Test-Path $venvPy)) {
    "[$(Get-Date -Format o)] venv missing; bootstrapping with setup.ps1 --doctor" | Out-File -FilePath $log -Append -Encoding utf8
    pwsh.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File (Join-Path $here 'setup.ps1') --doctor *>> $log
    if ($LASTEXITCODE -ne 0) {
      throw "Base setup failed with exit code $LASTEXITCODE"
    }
    if (-not (Test-Path $venvPy)) {
      throw "Base setup completed but venv python was not created: $venvPy"
    }
  }
  & $venvPy -c "import PySide6" *> $null
  if ($LASTEXITCODE -ne 0 -and (Test-Path $uiReq)) {
    "[$(Get-Date -Format o)] installing UI dependencies" | Out-File -FilePath $log -Append -Encoding utf8
    & $venvPy -m pip install --disable-pip-version-check --progress-bar raw -r $uiReq *>> $log
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
