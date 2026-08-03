# Installation

Use this page when the README's quick install is not enough.

## Windows 10 / 11

### Installer

Download the `.exe` installer from the
[latest release](https://github.com/FactusConsulting/whisper-dictate/releases/latest):

- `whisper-dictate-windows-setup-<version>.exe`

It installs to `%LOCALAPPDATA%\Programs\WhisperDictate`, adds the app to your
user PATH, and creates Start-menu/desktop shortcuts. The shortcut opens the
Rust UI, which starts and stops dictation, edits settings, shows logs, and
restarts the native runtime when needed.

Select a local Whisper model in **Settings > Speech recognition**, then download
it from the model section before starting dictation. An existing model file set
through `VOICEPI_WHISPER_MODEL_PATH` takes precedence over the download cache.

When `VOICEPI_LOCAL_ONLY=1` (or **Settings > System > Local-only mode**) is
enabled, network downloads are disabled. Install a catalog model manually into
the user cache, then select it in Settings:

- Windows: `%LOCALAPPDATA%\whisper-dictate\whisper-models\`
- Linux: `${XDG_CACHE_HOME:-~/.cache}/whisper-dictate/whisper-models/`
- macOS: `~/Library/Caches/whisper-dictate/whisper-models/`

The filename must match the catalog entry, for example
`ggml-large-v3.bin`. The application verifies the SHA-256 before treating the
file as available. Alternatively, disable local-only mode, use the model
download section once, and enable the privacy lock again afterward.

### Chocolatey

Windows quick install with Chocolatey CLI 2.x:

```powershell
choco source add -n=whisper-dictate -s="https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json"
choco install whisper-dictate --source=whisper-dictate -y
```

The public source does not require a GitHub account or token. It uses
Chocolatey CLI 2.x NuGet v3 feed support.

Common package commands:

```powershell
choco source list
choco install whisper-dictate --source=whisper-dictate -y
choco upgrade whisper-dictate --source=whisper-dictate -y
choco pin add -n=whisper-dictate
choco uninstall whisper-dictate -y
choco --version
```

The release workflow can also publish to GitHub Packages NuGet or another
configured feed through `CHOCOLATEY_NUGET_SOURCE` and
`CHOCOLATEY_NUGET_API_KEY`.

### Winget Local Manifests

The official `winget-pkgs` path is not active yet. Each release ships a
`whisper-dictate-winget-<version>.zip` asset with local manifests:

```powershell
winget settings --enable LocalManifestFiles
winget install --manifest .\winget
```

Download and extract the winget asset first, then point winget at the extracted
folder. The installer is unsigned, so SmartScreen may require **More info ->
Run anyway**.

### Portable Zip

Download the Windows zip from GitHub Releases, unzip it, then run:

```powershell
wd-gui.exe
```

`wd.exe` is the console CLI (every verb prints normally to
PowerShell). `wd-gui.exe` is the sibling tray/settings binary —
launch this one to start the UI without leaving a console window open
underneath it. The Inno installer's Start-menu / desktop shortcuts point at
`-gui.exe` for the same reason.

The `.exe` installer is still the recommended Windows path.

### Terminal Start

```powershell
wd run --key ctrl_r --lang da
wd.exe run --key ctrl_r --lang da --device vulkan
```

With the installer, the Start-menu **whisper-dictate** shortcut runs the Rust
UI and hosts the dictation runtime natively in-process — the hotkey listener,
coordinator, and session sink all run inside the Rust binary, no Python worker
child is spawned. Set `VOICEPI_LOG=debug` or `VOICEPI_LOG=trace` before launch
to capture progressively deeper native diagnostics in
`%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log`. The retired
`VOICEPI_DICTATE_ENGINE=python` value produces migration guidance.

## Ubuntu 24.04 / 26.04 - Wayland

Requires [Homebrew](https://brew.sh):

```bash
brew tap factusconsulting/tap
brew install whisper-dictate
wd setup-ubuntu
```

`setup-ubuntu` installs the Wayland input pieces, creates a GNOME
**Whisper Dictate** launcher, adds login autostart, and opens the Rust UI. Log
out and back in if the setup command adds you to the `input` group.

Then press **Start** in the Dictation tab. Hold **right Shift + right Ctrl**,
speak, release.

Terminal start:

```bash
wd run --key shift_r+ctrl_r --lang da
```

If Wayland hotkeys or injection fail:

```bash
wd doctor
```

## Linux X11

From a release zip:

```bash
unzip whisper-dictate-linux-<version>.zip
cd whisper-dictate
./wd ui
```

From a source checkout:

```bash
git clone https://github.com/FactusConsulting/whisper-dictate.git
cd whisper-dictate
scripts/linux/install-rust-ui.sh
wd ui
```

The source installer builds the complete native dictation route, including
global hotkeys, audio capture, injection, and local whisper.cpp. It checks the
required compiler and development libraries before building and prints the
missing prerequisites.

Ubuntu/Debian source-build and runtime dependencies:

```bash
sudo apt install \
  build-essential pkg-config cmake clang libclang-dev \
  libdbus-1-dev libwayland-dev libx11-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxi-dev libxtst-dev libasound2-dev \
  libportaudio2 alsa-utils xclip
```

To install the Rust desktop controller manually:

```bash
scripts/linux/install-rust-ui.sh
wd ui
```

The installer copies the Rust binary to `~/.local/bin/whisper-dictate` and
adds a desktop entry.

## NixOS / Nix

Run without installing:

```bash
nix run github:FactusConsulting/whisper-dictate -- run --key shift_r+ctrl_r --lang da
```

Install into a profile:

```bash
nix profile install github:FactusConsulting/whisper-dictate
wd run --key shift_r+ctrl_r --lang da
```

NixOS module:

```nix
inputs.whisper-dictate.url = "github:FactusConsulting/whisper-dictate";
```

```nix
imports = [ inputs.whisper-dictate.nixosModules.default ];

services.whisperDictate = {
  enable = true;
  users = [ "yourname" ];
};
```

The module enables Wayland text injection support, adds the udev rule for
`/dev/uinput`, and installs the package system-wide. Log out and back in after
the first activation if group membership changed.

## Verify Windows Downloads

Each release publishes `sha256sums.txt`.

```powershell
(Get-FileHash .\whisper-dictate-windows-setup-<version>.exe -Algorithm SHA256).Hash.ToLower()
```

Unsigned installers may trigger antivirus or SmartScreen warnings because the
app combines microphone capture, a global hotkey, and text injection. Verify the
SHA256 before allowing the installer.
