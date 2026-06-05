$ErrorActionPreference = 'Stop'

$packageName = 'whisper-dictate'
$productCode = '{7B3F8A2C-4E1D-4F9A-B5C6-D2E8F0A1C3B7}_is1'
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$productCode"
$uninstallString = $null

if (Test-Path $uninstallKey) {
  $uninstallString = (Get-ItemProperty $uninstallKey).UninstallString
}

if (-not $uninstallString) {
  $registryRoots = @(
    'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
  )
  foreach ($root in $registryRoots) {
    if (-not (Test-Path $root)) {
      continue
    }
    $match = Get-ChildItem $root |
      ForEach-Object { Get-ItemProperty $_.PSPath } |
      Where-Object { $_.DisplayName -eq 'whisper-dictate' } |
      Select-Object -First 1
    if ($match) {
      $uninstallString = $match.UninstallString
      break
    }
  }
}

if (-not $uninstallString) {
  Write-Warning "$packageName uninstall entry was not found."
  return
}

$uninstaller = $uninstallString.Trim('"')
$packageArgs = @{
  packageName    = $packageName
  fileType       = 'exe'
  file           = $uninstaller
  silentArgs     = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART'
  validExitCodes = @(0, 3010, 1605, 1614, 1641)
}

Uninstall-ChocolateyPackage @packageArgs

try {
  Uninstall-BinFile -Name $packageName
} catch {
  Write-Verbose "No Chocolatey shim to remove for $packageName."
}
