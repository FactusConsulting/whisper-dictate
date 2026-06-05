$ErrorActionPreference = 'Stop'

$packageName = 'whisper-dictate'
$installerUrl = '__INSTALLER_URL__'
$installerChecksum = '__INSTALLER_SHA256__'

$packageArgs = @{
  packageName    = $packageName
  fileType       = 'exe'
  url64bit       = $installerUrl
  checksum64     = $installerChecksum
  checksumType64 = 'sha256'
  silentArgs     = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART'
  validExitCodes = @(0, 3010, 1641)
}

Install-ChocolateyPackage @packageArgs

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\WhisperDictate'
$exePath = Join-Path $installDir 'whisper-dictate.exe'

if (Test-Path $exePath) {
  try {
    Uninstall-BinFile -Name $packageName
  } catch {
    Write-Verbose "No existing Chocolatey shim to remove for $packageName."
  }
  Install-BinFile -Name $packageName -Path $exePath
} else {
  Write-Warning "Expected whisper-dictate executable was not found at $exePath. The Start menu shortcut may still work, but the Chocolatey command shim was not created."
}
