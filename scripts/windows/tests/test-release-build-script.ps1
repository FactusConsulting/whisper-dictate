$ErrorActionPreference = 'Stop'
$builder = Join-Path $PSScriptRoot '..\build-release-binaries.ps1'
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('wd release build fixture ' + [guid]::NewGuid().ToString('N'))
$oldEnvironment = @{}
foreach ($key in @('RELEASE_FEATURE_RESOLVER', 'TAG', 'VOICEPI_BUILD_VULKAN', 'GITHUB_ENV', 'VULKAN_SDK', 'WD_RELEASE_FIXTURE_MODE', 'WD_RELEASE_FIXTURE_ONNX')) {
    $oldEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
Push-Location $fixtureRoot
try {
    $resolver = Join-Path $fixtureRoot 'resolver.ps1'
    [IO.File]::WriteAllText($resolver, @'
param([string]$ManifestPath, [string]$ReleaseTag)
if ($ManifestPath -ne 'src/rust/Cargo.toml' -or $ReleaseTag -ne 'v3.3.1') { throw 'Resolver argv changed' }
[pscustomobject]@{
  Mode = $env:WD_RELEASE_FIXTURE_MODE
  CpuFeatures = @('rust-hotkeys')
  VulkanFeatures = @()
  SupportsVulkan = $false
  OnnxRuntimeRequired = $env:WD_RELEASE_FIXTURE_ONNX -eq '1'
}
'@)
    New-Item -ItemType Directory -Path 'target\release', 'target\build\fixture', 'packaging\windows\inno' -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $fixtureRoot 'packaging\windows\inno\whisper-dictate.iss'), 'onnxruntime*.dll')
    [IO.File]::WriteAllText((Join-Path $fixtureRoot 'target\release\onnxruntime.dll'), 'fixture only')
    $cache = Join-Path $fixtureRoot 'target\build\fixture\CMakeCache.txt'
    [IO.File]::WriteAllText($cache, "GGML_NATIVE:BOOL=OFF`n")
    $env:RELEASE_FEATURE_RESOLVER = $resolver
    $env:TAG = 'v3.3.1'
    $env:GITHUB_ENV = Join-Path $fixtureRoot 'github-env.txt'
    $env:VOICEPI_BUILD_VULKAN = '0'
    $env:WD_RELEASE_FIXTURE_MODE = 'named'
    $global:WdReleaseFixtureExit = 0
    function cargo {
        $global:WdReleaseFixtureArgs = @($args)
        $global:LASTEXITCODE = $global:WdReleaseFixtureExit
    }
    & $builder
    $expected = @('build', '--manifest-path', 'src/rust/Cargo.toml', '--target-dir', 'target', '--release', '-p', 'whisper-dictate-app', '--bins', '--no-default-features', '--features', 'shipping')
    if (($global:WdReleaseFixtureArgs -join '|') -ne ($expected -join '|')) { throw 'Named CPU build argv changed' }
    if (-not ([IO.File]::ReadAllText($env:GITHUB_ENV).Contains('LEGACY_ONNX_REQUIRED=false'))) { throw 'Named sidecar gate changed' }

    $env:VOICEPI_BUILD_VULKAN = '1'
    $env:WD_RELEASE_FIXTURE_MODE = 'legacy'
    $env:WD_RELEASE_FIXTURE_ONNX = '1'
    & $builder
    $expected = @('build', '--manifest-path', 'src/rust/Cargo.toml', '--target-dir', 'target', '--release', '-p', 'whisper-dictate-app', '--bins', '--features', 'rust-hotkeys')
    if (($global:WdReleaseFixtureArgs -join '|') -ne ($expected -join '|')) { throw 'Legacy CPU build argv changed' }
    $exports = [IO.File]::ReadAllText($env:GITHUB_ENV)
    if (-not ($exports.Contains('VOICEPI_BUILD_VULKAN=0') -and $exports.Contains('LEGACY_ONNX_REQUIRED=true'))) { throw 'Legacy fallback/sidecar exports changed' }

    $global:WdReleaseFixtureExit = 17
    try { & $builder; throw 'Failed Cargo build was accepted' } catch {
        if ($_.Exception.Message -notmatch '^cargo build failed$') { throw }
    }
    $global:WdReleaseFixtureExit = 0
    [IO.File]::WriteAllText($cache, "GGML_NATIVE:BOOL=ON`n")
    try { & $builder; throw 'Host-native CMake flags were accepted' } catch {
        if ($_.Exception.Message -notmatch '^GGML_NATIVE must be OFF') { throw }
    }
    $env:WD_RELEASE_FIXTURE_MODE = 'named'
    $env:VOICEPI_BUILD_VULKAN = '1'
    Remove-Item Env:VULKAN_SDK -ErrorAction SilentlyContinue
    try { & $builder; throw 'GPU path skipped its SDK requirement' } catch {
        if ($_.Exception.Message -notmatch '^VULKAN_SDK not set') { throw }
    }
    Write-Host 'Release build script fixture PASS: CPU argv, legacy fallback/sidecars, build/native failures and GPU SDK gate'
} finally {
    Pop-Location
    foreach ($key in $oldEnvironment.Keys) { [Environment]::SetEnvironmentVariable($key, $oldEnvironment[$key], 'Process') }
    Remove-Variable WdReleaseFixtureArgs, WdReleaseFixtureExit -Scope Global -ErrorAction SilentlyContinue
    $resolved = [IO.Path]::GetFullPath($fixtureRoot)
    $tempPrefix = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]'\/') + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase) -or -not ([IO.Path]::GetFileName($resolved).StartsWith('wd release build fixture '))) { throw 'Refusing unsafe fixture cleanup' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
