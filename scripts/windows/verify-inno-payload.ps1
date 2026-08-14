param(
    [Parameter(Mandatory = $true)]
    [string]$InstallerPath,

    [Parameter(Mandatory = $true)]
    [string]$InstallDirectory,

    [Parameter(Mandatory = $true)]
    [string]$RequiredFilePattern
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Remove-DirectoryWithRetry {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $deadline = (Get-Date).AddSeconds(30)
    do {
        try {
            Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
            return
        } catch {
            if ((Get-Date) -ge $deadline) { throw }
            Start-Sleep -Milliseconds 250
        }
    } while (Test-Path -LiteralPath $Path)
}

$installer = (Resolve-Path -LiteralPath $InstallerPath).Path
$installRoot = [System.IO.Path]::GetFullPath($InstallDirectory)
$allowedRoots = @([System.IO.Path]::GetTempPath())
if ($env:RUNNER_TEMP) { $allowedRoots += $env:RUNNER_TEMP }
$insideAllowedRoot = $allowedRoots | Where-Object {
    $prefix = [System.IO.Path]::GetFullPath($_).TrimEnd('\') + '\'
    $installRoot.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)
}
if (-not $insideAllowedRoot) {
    throw "Inno verification directory must stay under the system temp root: $installRoot"
}
$originalUserPath = [Environment]::GetEnvironmentVariable('PATH', 'User')

try {
    if (Test-Path -LiteralPath $installRoot) {
        Remove-DirectoryWithRetry -Path $installRoot
    }
    $installProcess = Start-Process -FilePath $installer -ArgumentList @(
        '/VERYSILENT',
        '/SUPPRESSMSGBOXES',
        '/NORESTART',
        '/SP-',
        '/NOICONS',
        "/DIR=`"$installRoot`""
    ) -Wait -PassThru -NoNewWindow
    if ($installProcess.ExitCode -ne 0) {
        throw "Inno verification install failed with exit code $($installProcess.ExitCode)"
    }
    $required = @(Get-ChildItem -LiteralPath $installRoot -Filter $RequiredFilePattern -File)
    if ($required.Count -eq 0) {
        throw "Installed Inno payload is missing required file pattern $RequiredFilePattern"
    }
    Write-Host "Verified installed Inno payload: $($required.Name -join ', ')"
} finally {
    $uninstaller = Join-Path $installRoot 'unins000.exe'
    if (Test-Path -LiteralPath $uninstaller -PathType Leaf) {
        $uninstallProcess = Start-Process -FilePath $uninstaller -ArgumentList @(
            '/VERYSILENT',
            '/SUPPRESSMSGBOXES',
            '/NORESTART'
        ) -Wait -PassThru -NoNewWindow
        if ($uninstallProcess.ExitCode -ne 0) {
            throw "Inno verification uninstall failed with exit code $($uninstallProcess.ExitCode)"
        }
        $deadline = (Get-Date).AddSeconds(30)
        while ((Test-Path -LiteralPath $installRoot) -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 250
        }
    }
    [Environment]::SetEnvironmentVariable('PATH', $originalUserPath, 'User')
    if (Test-Path -LiteralPath $installRoot) {
        Remove-DirectoryWithRetry -Path $installRoot
    }
}
