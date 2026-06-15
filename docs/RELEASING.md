# Releasing

whisper-dictate ships through `.github/workflows/release.yml`, triggered by
pushing a version tag. There are two kinds of release:

- **Final** — `vX.Y.Z` (e.g. `v1.9.5`). A normal "Latest" GitHub release that
  every channel picks up.
- **Prerelease (RC)** — `vX.Y.Z-rc.N` (e.g. `v1.9.5-rc.1`). A GitHub
  *prerelease* that stable users do **not** get automatically. Used to gate a
  final on real-world testing.

The four version files must always agree — `VERSION`, `src/rust/Cargo.toml`,
`src/rust/Cargo.lock`, `nix/package.nix`. `scripts/dev/bump_version.py` writes
all four and refuses to start unless every file matches its expected pattern.

## Cut a release candidate (RC)

1. **Bump to the RC version** on a branch and open a version PR:

   ```bash
   python scripts/dev/bump_version.py 1.9.5-rc.1   # writes all four files
   # commit, push, open PR, merge (main is protected: PR + CI only)
   ```

   `bump_version.py` accepts `X.Y.Z-rc.N` (N >= 1). Cargo and Nix both accept
   the SemVer `-rc.N` suffix as a valid version string, so the RC version flows
   through unchanged. Malformed prereleases (`1.9.5-rc`, `1.9.5-rc.x`,
   `1.9.5-rc.0`) are rejected.

2. **Tag the merged commit** and push:

   ```bash
   git tag v1.9.5-rc.1 && git push origin v1.9.5-rc.1
   ```

   `release.yml` detects the `-rc.` in the tag (computed once as the
   `is_prerelease` job output) and publishes a **GitHub prerelease**.

### Which channels get the RC

| Channel       | RC behaviour                                                                 |
|---------------|------------------------------------------------------------------------------|
| **Chocolatey**| ✅ Published. The `1.9.5-rc.1` `.nupkg` is a NuGet/SemVer prerelease, added to the public Sleet feed. Install/upgrade with `--prerelease`: `choco upgrade whisper-dictate --prerelease --source=https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json`. A plain `choco upgrade` ignores it. |
| **Nix**       | ✅ Tag-based, no release.yml step needed — `package.nix` is in-repo, so the RC tag makes `nix run github:FactusConsulting/whisper-dictate/v1.9.5-rc.1#whisper-dictate` resolve at that rev. |
| **Homebrew**  | ⏭️ Skipped. The tap formula has a single "latest" version with no prerelease channel, so RCs are **finals-only**. |
| **winget**    | ⏭️ Skipped (no winget manifest asset is generated for an RC). winget has no prerelease channel and we don't auto-submit manifests, so it is **finals-only**. |
| **GitHub Release** | ✅ Published with `--prerelease` (hidden from "Latest release"). |

## Test the RC

1. **Automated — gate A (`install-smoke`)**: the `install-smoke` job in
   `release.yml` runs on a headless `windows-2025` runner after the release +
   installer are published, for **both** finals and RCs (it's cheap). It is a
   **post-publish gate**: it only *flags* a broken release (a red
   `install-smoke` check) — it never deletes or unpublishes the release.

   What it covers:
   - Downloads the just-published `whisper-dictate-windows-setup-<version>.exe`
     and **silently installs** it (Inno `/VERYSILENT /SUPPRESSMSGBOXES
     /NORESTART`).
   - Asserts the **installed layout**: `whisper-dictate.exe`, the worker
     entrypoint `src\python\whisper_dictate\runtime.py`, `benchmark\corpus.json`,
     the `data\hallucination_patterns.json` subpackage (the #226 packaging
     regression guard), and that the installed `VERSION` equals the tag.
   - Runs the **Rust controller headless** (`whisper-dictate.exe --version` and
     `config path`) and asserts it launches and exits 0 **without opening the
     UI window**. (The release binary is a Windows GUI-subsystem app, so only
     the exit code is reliable — its stdout is not attached to a redirected
     console.)
   - Creates a minimal venv (**only `sounddevice` + `numpy`** — no
     torch/faster-whisper/CUDA) and runs the installed worker's **no-model**
     audio query modes (`--test-audio-device ""` and `--list-audio-devices`),
     asserting they exit cleanly with parseable JSON rather than a traceback —
     proving the installed worker package imports and runs.

   What it deliberately does **not** cover (still manual — see step 2): real
   microphone capture / inject / post end-to-end (a cloud VM has no audio
   device), and a real STT model load (the heavy ML deps are not installed).
   If the installer build was skipped (no Windows-relevant changes since the
   previous tag, so no setup `.exe` was uploaded), the job skips itself.

2. **Manual real-world test**: install the RC and run real dictation on actual
   microphones (including the Blue Yeti) — confirm capture → inject → post
   end-to-end. Real-microphone testing cannot run on a headless cloud VM (no
   audio device), so this stays manual / on a dedicated physical box.

   ```powershell
   choco upgrade whisper-dictate --prerelease `
     --source=https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json
   ```

## Promote an RC to final

Once the RC passes testing, cut the final by dropping the `-rc.N` suffix:

```bash
python scripts/dev/bump_version.py 1.9.5    # all four files -> 1.9.5
# commit, push, open PR, merge
git tag v1.9.5 && git push origin v1.9.5
```

The final tag has no `-rc.`, so `is_prerelease` is `false`: the GitHub release
is a normal "Latest" release, Homebrew and winget run as before, and Chocolatey
publishes a stable `.nupkg` that plain `choco upgrade` picks up. **The stable
release path is unchanged** — every prerelease behaviour is additive and guarded
behind the `is_prerelease` flag.

## Local Windows Installer Loop

For a faster local Windows test loop without creating a release:

```powershell
.\scripts\windows\build-installer.ps1
```

The local installer and portable ZIP are written to `Output\`. The script uses
Inno Setup 6 and installs it via winget, or Chocolatey as a fallback, when it is
missing.

Local builds default to the latest release version plus unique SemVer build
metadata, for example `<version>+local.<timestamp>.g<sha>.dirty`, so they are
easy to distinguish from online releases and from each other. The installer
keeps a numeric Windows file version internally, for example
`<major>.<minor>.<patch>.1`.

The release workflow publishes the Linux bundle and Rust UI binary, then builds
the unified Windows installer, portable Windows ZIP bundle, and Chocolatey
package on a Windows runner.
