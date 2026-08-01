# Build Windows installers locally without creating a GitHub release.
param(
  [string]$Version = ''
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $root

function Get-CrateVersion {
  $cargoToml = Join-Path $root 'src\rust\Cargo.toml'
  $match = Select-String -LiteralPath $cargoToml -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
  if (-not $match) {
    throw "Could not read package version from $cargoToml"
  }
  return $match.Matches[0].Groups[1].Value
}

function Get-LocalBuildMetadata {
  $stamp = Get-Date -Format 'yyyyMMddHHmmss'
  $sha = ''
  $dirty = ''
  if (Get-Command git -ErrorAction SilentlyContinue) {
    $sha = (& git -C $root rev-parse --short HEAD 2>$null)
    if (-not [string]::IsNullOrWhiteSpace($sha)) {
      $sha = ".g$($sha.Trim())"
    }
    $status = (& git -C $root status --porcelain 2>$null)
    if ($status) {
      $dirty = ".dirty"
    }
  }
  return "local.$stamp$sha$dirty"
}

function Get-VersionInfoVersion([string]$DisplayVersion) {
  # Strip a semver prerelease suffix (e.g. `-rc.1`) before pattern-matching,
  # mirroring what `.github/workflows/windows-installer-build.yml` does for
  # VERSION_INFO. Inno Setup's VersionInfoVersion is a Windows
  # `FILEVERSION`/`PRODUCTVERSION` field that only accepts numeric dotted
  # forms; the prerelease tag is fine in AppVersion / the output filename
  # but breaks the resource compile. Without this strip the local Windows
  # installer loop blows up during the RC window (P2 #406 Codex finding).
  #
  # The lookahead `(?=$|\+)` matches `-rc.N` whether it terminates the
  # version (`1.19.0-rc.1`) or precedes local build metadata
  # (`1.19.0-rc.1+local.20260603073512.gabc1234` — the default path when
  # `-Version` is not passed during the RC window).
  $numeric = $DisplayVersion -replace '-rc\.\d+(?=$|\+)',''
  if ($numeric -match '^(\d+\.\d+\.\d+)\.(\d+)$') {
    return $numeric
  }
  if ($numeric -match '^(\d+\.\d+\.\d+)\+') {
    return "$($Matches[1]).1"
  }
  if ($numeric -match '^(\d+\.\d+\.\d+)$') {
    return $numeric
  }
  throw "Version must be numeric, semver build metadata, or semver -rc.N, e.g. 0.3.25, 0.3.25.1, 0.3.25+local.20260603073512.gabc1234, or 1.19.0-rc.1. Got: $DisplayVersion"
}

if (-not $Version) {
  $Version = "$(Get-CrateVersion)+$(Get-LocalBuildMetadata)"
}
$versionInfo = Get-VersionInfoVersion $Version

function Find-Iscc {
  $candidates = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
  )
  foreach ($path in $candidates) {
    if ($path -and (Test-Path $path)) { return $path }
  }
  $cmd = Get-Command iscc.exe -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  return $null
}

$iscc = Find-Iscc
if (-not $iscc) {
  if (Get-Command winget -ErrorAction SilentlyContinue) {
    Write-Host "Installing Inno Setup 6 via winget..." -ForegroundColor Cyan
    winget install -e --id JRSoftware.InnoSetup --scope user --silent `
      --accept-package-agreements --accept-source-agreements
    $iscc = Find-Iscc
  }
}
if (-not $iscc) {
  if (Get-Command choco -ErrorAction SilentlyContinue) {
    Write-Host "Installing Inno Setup 6 via Chocolatey..." -ForegroundColor Cyan
    choco install innosetup -y --no-progress
    $iscc = Find-Iscc
  }
}
if (-not $iscc) {
  throw "Inno Setup compiler ISCC.exe was not found. Install Inno Setup 6, then rerun this script."
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw "cargo was not found. Install Rust, then rerun this script."
}
Write-Host "Building Rust desktop UI..." -ForegroundColor Cyan
# Keep this --features list in lockstep with .github/workflows/windows-installer-build.yml
# so local installer builds match the artifact CI ships. P2 #400 Codex finding 4.
# Vulkan GPU acceleration (whisper-rs-vulkan) is appended when the Vulkan SDK is
# installed locally ($env:VULKAN_SDK is set by LunarG's installer, e.g.
# `C:\VulkanSDK\1.3.290.0`). Without the SDK the build falls back to CPU-only
# to keep the local loop green on dev machines that never installed it.
if ($env:VOICEPI_BUILD_VULKAN -eq '0') {
  # ASCII hyphens only in Write-Host output -- Windows PowerShell 5.1 and
  # cmd.exe relay can mangle em-dashes into `??` in hidden-launcher logs.
  # Codex P2 #647 discussion r3661216200.
  Write-Host "VOICEPI_BUILD_VULKAN=0 - CPU-only build (skipping whisper-rs-vulkan)" -ForegroundColor Yellow
  cargo build --manifest-path (Join-Path $root 'src\rust\Cargo.toml') --target-dir (Join-Path $root 'target') --release -p whisper-dictate-app --bins --features rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local
} elseif ($env:VULKAN_SDK -and (Test-Path (Join-Path $env:VULKAN_SDK 'Bin\glslc.exe'))) {
  $env:PATH = (Join-Path $env:VULKAN_SDK 'Bin') + ';' + $env:PATH
  Write-Host "VULKAN_SDK=$env:VULKAN_SDK - building with whisper-rs-vulkan (GPU acceleration)" -ForegroundColor Cyan
  # Mirror the CI workflow's Ninja generator override for the Vulkan build
  # (see .github/workflows/windows-installer-build.yml "Build Rust desktop UI"
  # step comment for the full rationale). Locally the developer is assumed to
  # be running from a "Developer PowerShell for VS 2022" (or equivalent) so
  # cl.exe and ninja are already on PATH; if not, print a hint and bail early
  # rather than fall into whisper-rs-sys's cryptic
  # CMakeTestCCompiler / MSB8066 nested-build failure.
  $ninja = (Get-Command ninja -ErrorAction SilentlyContinue).Source
  if (-not $ninja) {
    throw @"
ninja.exe not on PATH. The Vulkan build forces CMAKE_GENERATOR=Ninja to work
around whisper.cpp's vulkan-shaders-gen ExternalProject sub-build tripping on
MSBuild-in-MSBuild. Launch this script from a `Developer PowerShell for VS
2022` prompt (bundles Ninja + vcvars) or `choco install ninja -y` and re-run
from a vcvars-activated shell. Set VOICEPI_BUILD_VULKAN=0 to skip Vulkan.
"@
  }
  $env:CMAKE_GENERATOR = 'Ninja'
  Write-Host "ninja = $ninja" -ForegroundColor Cyan
  Write-Host "CMAKE_GENERATOR = $env:CMAKE_GENERATOR (forced Ninja to avoid MSBuild-in-MSBuild in the vulkan-shaders-gen sub-build)" -ForegroundColor Cyan
  # Same short-CARGO_TARGET_DIR fix as the CI workflow ("Build Rust desktop UI"
  # step in .github/workflows/windows-installer-build.yml). whisper.cpp's
  # vulkan-shaders-gen ExternalProject_Add drops a cmake compiler probe at
  # <target>/release/build/whisper-rs-sys-<hash>/out/build/ggml/src/
  # ggml-vulkan/vulkan-shaders-gen-prefix/src/vulkan-shaders-gen-build/
  # CMakeFiles/CMakeScratch/TryCompile-<id>/CMakeFiles/cmTC_<id>.dir/
  # testCCompiler.c.obj which pushes past Windows' MAX_PATH ~260 cap when
  # the workspace root is anything longer than a couple of segments, and
  # cl.exe then fails with `fatal error C1083: Cannot open compiler
  # generated file: '': Invalid argument` (empty output name = truncated).
  # A dev with a deep project path (e.g. D:\source\projects\voicepi\
  # whisper-dictate\) hits the same wall as CI. Point cargo at a short
  # target root and copy the artefacts back to $root\target\release\ so
  # the rest of the script (Inno .iss + ZIP bundle) still finds them at
  # the conventional location. If the developer has already set
  # CARGO_TARGET_DIR themselves, respect it -- they know what they're doing.
  $shortTargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'C:\t' }
  # Snapshot the pre-existing CARGO_TARGET_DIR presence + value so the
  # `finally` below restores exactly what the developer had. Without this
  # the assignment `$env:CARGO_TARGET_DIR = $shortTargetDir` leaks into
  # the enclosing PowerShell session: every subsequent `cargo` command in
  # the same terminal -- including commands for other repositories --
  # would silently share `C:\t`, causing hard-to-diagnose "why is my
  # target dir wrong" build weirdness (Codex P2 #670 review comment on
  # build-installer.ps1:158). `Test-Path env:...` distinguishes "unset"
  # from "empty string" so the restore never re-introduces a bogus empty
  # value for a variable the developer never touched.
  $prevCargoTargetDirWasSet = Test-Path env:CARGO_TARGET_DIR
  $prevCargoTargetDir = if ($prevCargoTargetDirWasSet) { $env:CARGO_TARGET_DIR } else { $null }
  try {
    $env:CARGO_TARGET_DIR = $shortTargetDir
    Write-Host "CARGO_TARGET_DIR = $env:CARGO_TARGET_DIR (short path to keep vulkan-shaders-gen TryCompile below Windows MAX_PATH)" -ForegroundColor Cyan
    cargo build --manifest-path (Join-Path $root 'src\rust\Cargo.toml') --target-dir $shortTargetDir --release -p whisper-dictate-app --bins --features rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local,whisper-rs-vulkan
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    # Copy release artefacts back to $root\target\release\ so the .iss +
    # ZIP steps below (which reference `target\release\...` relative to
    # $root) and the packaging/windows/inno/whisper-dictate.iss
    # `..\..\..\target\release\...` Source lines keep working unchanged.
    $conventionalRelease = Join-Path $root 'target\release'
    New-Item -ItemType Directory -Force $conventionalRelease | Out-Null
    Copy-Item (Join-Path $shortTargetDir 'release\wd.exe')     $conventionalRelease -Force
    Copy-Item (Join-Path $shortTargetDir 'release\wd-gui.exe') $conventionalRelease -Force
    $onnxDlls = @(Get-ChildItem (Join-Path $shortTargetDir 'release') -Filter 'onnxruntime*.dll' -ErrorAction SilentlyContinue)
    foreach ($dll in $onnxDlls) {
      Copy-Item $dll.FullName $conventionalRelease -Force
    }
  } finally {
    if ($prevCargoTargetDirWasSet) {
      $env:CARGO_TARGET_DIR = $prevCargoTargetDir
    } else {
      Remove-Item env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    }
  }
} else {
  Write-Host "Vulkan SDK not detected (`$env:VULKAN_SDK unset or `$env:VULKAN_SDK\Bin\glslc.exe missing) - building CPU-only." -ForegroundColor Yellow
  Write-Host "  Install from https://vulkan.lunarg.com/sdk/home to build a GPU-accelerated artefact locally." -ForegroundColor Yellow
  cargo build --manifest-path (Join-Path $root 'src\rust\Cargo.toml') --target-dir (Join-Path $root 'target') --release -p whisper-dictate-app --bins --features rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local
}
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$versionFile = Join-Path $root 'VERSION'
$hadVersion = Test-Path $versionFile
$oldVersion = if ($hadVersion) { Get-Content $versionFile -Raw } else { $null }
Set-Content $versionFile $Version -Encoding ascii -NoNewline
$outDir = Join-Path $root 'Output'
New-Item -ItemType Directory -Force $outDir | Out-Null

try {
  Write-Host "Building unified Windows installer version $Version (file version $versionInfo)..." -ForegroundColor Cyan
  & $iscc /DVERSION=$Version /DVERSION_INFO=$versionInfo /O"$outDir" packaging\windows\inno\whisper-dictate.iss
  if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

  Write-Host "Building unified Windows portable ZIP version $Version..." -ForegroundColor Cyan
  $zipRoot = Join-Path $outDir "zip-windows"
  $bundle = Join-Path $zipRoot 'whisper-dictate'
  Remove-Item -LiteralPath $zipRoot -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force $bundle | Out-Null
  $bundleConfig = Join-Path $bundle 'shared\config'
  New-Item -ItemType Directory -Force $bundleConfig | Out-Null
  Copy-Item -LiteralPath (Join-Path $root 'shared\config\settings_schema.json') -Destination $bundleConfig
  Copy-Item -LiteralPath (Join-Path $root 'README.md'), (Join-Path $root 'LICENSE'), $versionFile -Destination $bundle
  Copy-Item -LiteralPath (Join-Path $root 'docs') -Destination $bundle -Recurse
  $assetDir = Join-Path $bundle 'assets'
  New-Item -ItemType Directory -Force $assetDir | Out-Null
  Copy-Item -LiteralPath (Join-Path $root 'assets\whisper-dictate.ico') -Destination $assetDir
  Copy-Item -LiteralPath (Join-Path $root 'target\release\wd.exe') -Destination $bundle
  # Sibling windows-subsystem binary — shipped alongside the CLI so portable-zip
  # users get the tray-launch UX (no cmd-window flash) that the Inno installer's
  # shortcuts already provide.
  Copy-Item -LiteralPath (Join-Path $root 'target\release\wd-gui.exe') -Destination $bundle
  # Ship the golden-benchmark manifest (corpus.json only — NOT the user-local,
  # gitignored audio) so "Run benchmark" resolves a corpus out of the box.
  $benchmarkDir = Join-Path $bundle 'benchmark'
  New-Item -ItemType Directory -Force $benchmarkDir | Out-Null
  Copy-Item -LiteralPath (Join-Path $root 'benchmark\corpus.json') -Destination $benchmarkDir
  $zipPath = Join-Path $outDir "whisper-dictate-windows-$Version.zip"
  Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
  Compress-Archive -Path $bundle -DestinationPath $zipPath -CompressionLevel Optimal
} finally {
  if ($hadVersion) {
    Set-Content $versionFile $oldVersion.TrimEnd("`r", "`n") -Encoding ascii -NoNewline
  } else {
    Remove-Item -LiteralPath $versionFile -ErrorAction SilentlyContinue
  }
}

Get-ChildItem $outDir -Filter "whisper-dictate-windows-setup-$Version.exe" |
  Select-Object FullName, Length
Get-ChildItem $outDir -Filter "whisper-dictate-windows-$Version.zip" |
  Select-Object FullName, Length
