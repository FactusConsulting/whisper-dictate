$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Remove-TestDirectoryWithRetry {
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

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..')).Path
$resolver = Join-Path $repoRoot 'scripts\windows\resolve-release-features.ps1'
$portableBuilder = Join-Path $repoRoot 'scripts\windows\build-portable-zip.ps1'
$innoVerifier = Join-Path $repoRoot 'scripts\windows\verify-inno-payload.ps1'
foreach ($helper in @($resolver, $portableBuilder, $innoVerifier)) {
    if (-not (Test-Path -LiteralPath $helper -PathType Leaf)) {
        throw "Legacy installer integration helper is missing: $helper"
    }
}

$isccCandidates = @(
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
    'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
)
$iscc = $isccCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
    Select-Object -First 1
if (-not $iscc) {
    throw 'Inno Setup 6 is required for the legacy installer integration test'
}

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    'whisper-dictate legacy installer ' + [guid]::NewGuid().ToString('N')
)
$sourceRoot = Join-Path $testRoot 'source'
$outputDir = Join-Path $testRoot 'output'
$installDir = Join-Path $testRoot 'installed'
$tagArchive = Join-Path $testRoot 'v1.25.0.zip'
$originalUserPath = [Environment]::GetEnvironmentVariable('PATH', 'User')

try {
    New-Item -ItemType Directory -Force -Path $sourceRoot, $outputDir | Out-Null
    git -C $repoRoot archive --format=zip --output=$tagArchive v1.25.0
    if ($LASTEXITCODE -ne 0) { throw 'git archive v1.25.0 failed' }
    Expand-Archive -LiteralPath $tagArchive -DestinationPath $sourceRoot

    $plan = & $resolver `
        -ManifestPath (Join-Path $sourceRoot 'src\rust\Cargo.toml') `
        -ReleaseTag v1.25.0
    if ($plan.Mode -ne 'legacy' -or -not $plan.OnnxRuntimeRequired) {
        throw 'v1.25.0 did not resolve to the expected legacy ONNX release plan'
    }

    $releaseDir = Join-Path $sourceRoot 'target\release'
    New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $releaseDir 'wd.exe'), 'test cli')
    [System.IO.File]::WriteAllText((Join-Path $releaseDir 'wd-gui.exe'), 'test gui')
    [System.IO.File]::WriteAllText(
        (Join-Path $releaseDir 'onnxruntime-test.dll'),
        'test onnx sidecar'
    )

    # Isolate the integration install from any real local installation.
    $issPath = Join-Path $sourceRoot 'packaging\windows\inno\whisper-dictate.iss'
    $iss = Get-Content -LiteralPath $issPath -Raw
    $iss = $iss.Replace(
        '7B3F8A2C-4E1D-4F9A-B5C6-D2E8F0A1C3B7',
        'E2FDFE6E-885A-4E5D-A98B-3B206073719A'
    )
    Set-Content -LiteralPath $issPath -Value $iss -Encoding utf8

    & $iscc /DVERSION=1.25.0 /DVERSION_INFO=1.25.0 "/O$outputDir" $issPath
    if ($LASTEXITCODE -ne 0) { throw 'legacy Inno fixture build failed' }
    $installer = Join-Path $outputDir 'whisper-dictate-windows-setup-1.25.0.exe'
    & $innoVerifier `
        -InstallerPath $installer `
        -InstallDirectory $installDir `
        -RequiredFilePattern 'onnxruntime*.dll'

    $zip = & $portableBuilder `
        -SourceRoot $sourceRoot `
        -OutputDirectory $outputDir `
        -Version 1.25.0 `
        -LegacyOnnxRequired
    if (-not (Test-Path -LiteralPath $zip -PathType Leaf)) {
        throw "portable ZIP was not produced: $zip"
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
    try {
        $names = @($archive.Entries | ForEach-Object { $_.Name })
        foreach ($required in @('wd.exe', 'wd-gui.exe', 'onnxruntime-test.dll')) {
            if ($names -notcontains $required) {
                throw "legacy portable ZIP is missing $required"
            }
        }
    } finally {
        $archive.Dispose()
    }

    $modernZip = & $portableBuilder `
        -SourceRoot $sourceRoot `
        -OutputDirectory $outputDir `
        -Version current-test
    $modernArchive = [System.IO.Compression.ZipFile]::OpenRead($modernZip)
    try {
        $unexpectedOnnx = $modernArchive.Entries |
            Where-Object { $_.Name -like 'onnxruntime*.dll' }
        if ($unexpectedOnnx) {
            throw 'current portable ZIP resurrected a stale ONNX Runtime sidecar'
        }
    } finally {
        $modernArchive.Dispose()
    }
    Write-Host 'OK legacy v1.25.0 installer and portable ZIP payloads'
} finally {
    [Environment]::SetEnvironmentVariable('PATH', $originalUserPath, 'User')
    if (Test-Path -LiteralPath $testRoot) {
        $resolvedTestRoot = (Resolve-Path -LiteralPath $testRoot).Path
        $resolvedTempRoot = (Resolve-Path -LiteralPath ([System.IO.Path]::GetTempPath())).Path
        if (-not $resolvedTestRoot.StartsWith($resolvedTempRoot, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove integration-test path outside temp: $resolvedTestRoot"
        }
        Remove-TestDirectoryWithRetry -Path $resolvedTestRoot
    }
}
