# Build binaries with the reviewed feature resolver, portable CPU flags and short Vulkan target.
$ErrorActionPreference = 'Stop'
$featurePlan = & $env:RELEASE_FEATURE_RESOLVER `
  -ManifestPath src/rust/Cargo.toml `
  -ReleaseTag $env:TAG
if ($featurePlan.Mode -eq 'named') {
  $vulkanFeatureArgs = @('--no-default-features', '--features', 'shipping-vulkan')
  $cpuFeatureArgs = @('--no-default-features', '--features', 'shipping')
} else {
  Write-Host "Tag predates shipping profiles; resolved legacy CPU features: $($featurePlan.CpuFeatures -join ',')"
  $cpuFeatureArgs = @()
  if ($featurePlan.CpuFeatures.Count -gt 0) {
    $cpuFeatureArgs = @('--features', ($featurePlan.CpuFeatures -join ','))
  }
  $vulkanFeatureArgs = @()
  if ($featurePlan.VulkanFeatures.Count -gt 0) {
    $vulkanFeatureArgs = @('--features', ($featurePlan.VulkanFeatures -join ','))
  }
  if (-not $featurePlan.SupportsVulkan -and $env:VOICEPI_BUILD_VULKAN -ne '0') {
    Write-Host 'Checked-out tag has no whisper-rs-vulkan feature; using its CPU release surface'
    $env:VOICEPI_BUILD_VULKAN = '0'
    'VOICEPI_BUILD_VULKAN=0' | Add-Content -LiteralPath $env:GITHUB_ENV -Encoding utf8
  }
}
$legacyOnnxRequired = $featurePlan.Mode -eq 'legacy' -and $featurePlan.OnnxRuntimeRequired
"LEGACY_ONNX_REQUIRED=$($legacyOnnxRequired.ToString().ToLowerInvariant())" |
  Add-Content -LiteralPath $env:GITHUB_ENV -Encoding utf8
if ($legacyOnnxRequired -and -not (
  Select-String -LiteralPath packaging/windows/inno/whisper-dictate.iss -Pattern 'onnxruntime\*\.dll' -Quiet
)) {
  throw 'This legacy tag enables audio-in-rust but its Inno recipe does not package the required ONNX Runtime sidecar.'
}
$builtReleaseDir = Join-Path $PWD 'target\release'
$ggmlBuildTarget = Join-Path $PWD 'target'
if ($env:VOICEPI_BUILD_VULKAN -ne '0') {
  if (-not $env:VULKAN_SDK) { throw "VULKAN_SDK not set - the 'Install Vulkan SDK' step did not run or failed silently" }
  $env:PATH = (Join-Path $env:VULKAN_SDK 'Bin') + ';' + $env:PATH
  Write-Host "VULKAN_SDK = $env:VULKAN_SDK"
  $glslc = (Get-Command glslc -ErrorAction SilentlyContinue).Source
  if (-not $glslc) { throw "glslc not on PATH after VULKAN_SDK\Bin prepend - SDK layout regression" }
  Write-Host "glslc = $glslc"
  $env:CMAKE_GENERATOR = 'Ninja'
  $ninja = (Get-Command ninja -ErrorAction SilentlyContinue).Source
  if (-not $ninja) { throw "ninja not on PATH - the 'Set up MSVC developer environment' step did not run or the VS 2022 Ninja shipment moved" }
  Write-Host "ninja = $ninja"
  Write-Host "CMAKE_GENERATOR = $env:CMAKE_GENERATOR (forced Ninja to avoid MSBuild-in-MSBuild in the vulkan-shaders-gen sub-build)"
  $shortTargetDir = 'D:\t'
  $env:CARGO_TARGET_DIR = $shortTargetDir
  $ggmlBuildTarget = $shortTargetDir
  Write-Host "CARGO_TARGET_DIR = $env:CARGO_TARGET_DIR (short path to keep vulkan-shaders-gen TryCompile below Windows MAX_PATH)"
  Write-Host "Building whisper-dictate with Vulkan GPU acceleration for whisper.cpp"
  cargo build --manifest-path src/rust/Cargo.toml --target-dir $shortTargetDir --release -p whisper-dictate-app --bins @vulkanFeatureArgs
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
  $builtReleaseDir = Join-Path $shortTargetDir 'release'
  New-Item -ItemType Directory -Force target\release | Out-Null
  Copy-Item (Join-Path $shortTargetDir 'release\wd.exe')     target\release\ -Force
  Copy-Item (Join-Path $shortTargetDir 'release\wd-gui.exe') target\release\ -Force
} else {
  Write-Host "VOICEPI_BUILD_VULKAN=0 - building whisper-dictate WITHOUT GPU acceleration (CPU-only fallback)"
  cargo build --manifest-path src/rust/Cargo.toml --target-dir target --release -p whisper-dictate-app --bins @cpuFeatureArgs
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}
$ggmlCaches = @(
  Get-ChildItem -LiteralPath $ggmlBuildTarget -Recurse -Filter CMakeCache.txt |
    Where-Object {
      Select-String -LiteralPath $_.FullName -Pattern '^GGML_NATIVE:' -Quiet
    }
)
if ($ggmlCaches.Count -eq 0) {
  throw "whisper.cpp CMake cache missing under $ggmlBuildTarget; cannot verify portable CPU build"
}
$nativeCaches = @(
  $ggmlCaches | Where-Object {
    -not (Select-String -LiteralPath $_.FullName -Pattern '^GGML_NATIVE:BOOL=OFF$' -Quiet)
  }
)
if ($nativeCaches.Count -ne 0) {
  throw "GGML_NATIVE must be OFF in every whisper.cpp CMake cache: $($nativeCaches.FullName -join ', ')"
}
Write-Host "OK portable whisper.cpp CPU build - GGML_NATIVE=OFF in $($ggmlCaches.Count) CMake cache(s)"
if ($legacyOnnxRequired) {
  $onnxDlls = @(Get-ChildItem -LiteralPath $builtReleaseDir -Filter 'onnxruntime*.dll' -File)
  if ($onnxDlls.Count -eq 0) {
    throw "Legacy audio-in-rust build did not emit its required ONNX Runtime DLL in $builtReleaseDir"
  }
  New-Item -ItemType Directory -Force target\release | Out-Null
  foreach ($dll in $onnxDlls) {
    $destination = Join-Path (Join-Path $PWD 'target\release') $dll.Name
    if ($dll.FullName -ne $destination) {
      Copy-Item -LiteralPath $dll.FullName -Destination $destination -Force
    }
  }
  Write-Host "Staged legacy ONNX Runtime sidecars: $($onnxDlls.Name -join ', ')"
}
