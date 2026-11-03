# Install the pinned LunarG Vulkan SDK for the Windows release build and export
# VULKAN_SDK / VULKAN_VERSION / PATH for the later workflow steps.
#
# The installer is downloaded from the repository's toolchain mirror release
# first and from sdk.lunarg.com second; a download only counts when it matches
# the pinned SHA-256. Each call starts from a clean destination so a retry after
# a timed-out or failed attempt never builds against a partial SDK.
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [Parameter(Mandatory = $true)][string]$Sha256,
  [Parameter(Mandatory = $true)][string]$MirrorUrl,
  [string]$Destination = "C:\VulkanSDK\$Version"
)
$ErrorActionPreference = 'Stop'

$installerName = "vulkansdk-windows-X64-$Version.exe"
$sources = @(
  $MirrorUrl,
  "https://sdk.lunarg.com/sdk/download/$Version/windows/$installerName"
)
$installer = Join-Path $env:RUNNER_TEMP $installerName

# A previous attempt killed by the step timeout can leave its installer running
# and holding files under the destination.
Get-Process -Name ([System.IO.Path]::GetFileNameWithoutExtension($installerName)) -ErrorAction SilentlyContinue |
  Stop-Process -Force -ErrorAction SilentlyContinue

$verified = $false
foreach ($url in $sources) {
  Write-Host "Downloading Vulkan SDK $Version from $url"
  Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue
  # Abort a stalled transfer (below 256 KiB/s for 60 s) so the next source
  # still fits inside the step timeout.
  & curl.exe --fail --location --silent --show-error `
    --retry 2 --retry-all-errors --connect-timeout 30 `
    --speed-limit 262144 --speed-time 60 `
    --output $installer $url
  if ($LASTEXITCODE -ne 0) {
    Write-Warning "Download failed (curl exit $LASTEXITCODE): $url"
    continue
  }
  $actual = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($actual -ne $Sha256.ToLowerInvariant()) {
    Write-Warning "SHA-256 mismatch for ${url}: expected $Sha256, got $actual"
    continue
  }
  Write-Host "OK SHA-256 $actual"
  $verified = $true
  break
}
if (-not $verified) {
  throw "Could not download a Vulkan SDK $Version installer matching SHA-256 $Sha256 from: $($sources -join ', ')"
}

if (Test-Path -LiteralPath $Destination) {
  Remove-Item -LiteralPath $Destination -Recurse -Force
}
# Same unattended arguments LunarG documents for the Qt installer framework.
$arguments = @('--root', $Destination, '--accept-licenses', '--default-answer', '--confirm-command', 'install')
$process = Start-Process -FilePath $installer -ArgumentList $arguments -Wait -PassThru -NoNewWindow
if ($process.ExitCode -ne 0) {
  throw "Vulkan SDK installer exited with code $($process.ExitCode)"
}
Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue

foreach ($required in @('Bin\glslc.exe', 'Include\vulkan\vulkan.h')) {
  $path = Join-Path $Destination $required
  if (-not (Test-Path -LiteralPath $path)) {
    throw "Vulkan SDK install is incomplete: $path is missing"
  }
}
$importLib = Get-ChildItem -LiteralPath (Join-Path $Destination 'Lib') -Recurse -Filter 'vulkan-1.lib' -File -ErrorAction SilentlyContinue |
  Select-Object -First 1
if (-not $importLib) {
  throw "Vulkan SDK install is incomplete: no vulkan-1.lib under $(Join-Path $Destination 'Lib')"
}
Write-Host "vulkan-1.lib = $($importLib.FullName)"

"VULKAN_SDK=$Destination" | Add-Content -LiteralPath $env:GITHUB_ENV -Encoding utf8
"VULKAN_VERSION=$Version" | Add-Content -LiteralPath $env:GITHUB_ENV -Encoding utf8
(Join-Path $Destination 'Bin') | Add-Content -LiteralPath $env:GITHUB_PATH -Encoding utf8
Write-Host "OK Vulkan SDK $Version installed at $Destination"
