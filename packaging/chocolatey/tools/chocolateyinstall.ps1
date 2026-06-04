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
