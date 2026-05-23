# Changelog

All notable changes to whisper-dictate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Entries below v0.2.36 were generated retroactively from `git log` per
tag; chore-bump lines (winget manifests, nix/package.nix) trail each
release because `release.yml` bumps in-place after the tag is pushed.

## [Unreleased]

### Changed
- Soft-pinned `sounddevice`, `pynput`, `pyperclip`, `evdev` to major-compatible ranges in both `requirements-cpu.txt` and `requirements-gpu.txt`.

### Fixed
- `DeviceResolutionTests` now isolates `VOICEPI_COMPUTE_TYPE` so the suite passes on machines where the user has the env var set.
- `AudioDspTests` skips cleanly when numpy is not installed (previously crashed `setUpClass`).

## [0.2.35] - 2026-05-23

- fix(setup.ps1): stop --paste from silently overriding VOICEPI_INJECT_MODE
- feat(scripts): add probe-key.py to verify a hotkey before setx
- chore: update winget manifests for v0.2.34
- chore: bump nix/package.nix to 0.2.34

## [0.2.34] - 2026-05-22

- feat(config): add env settings for hotkey and injection
- chore: update winget manifests for v0.2.33
- chore: bump nix/package.nix to 0.2.33

## [0.2.33] - 2026-05-20

- feat(debug): VOICEPI_DEBUG dumps every effective setting at startup
- docs(config): add GPU VRAM sizing table
- chore: update winget manifests for v0.2.32
- chore: bump nix/package.nix to 0.2.32

## [0.2.32] - 2026-05-20

- feat(device): add VOICEPI_COMPUTE_TYPE env override
- chore: update winget manifests for v0.2.31
- chore: bump nix/package.nix to 0.2.31

## [0.2.31] - 2026-05-20

- fix: require 3 consecutive Esc to quit (configurable); avoids global-Esc footgun
- chore: update winget manifests for v0.2.30
- chore: bump nix/package.nix to 0.2.30

## [0.2.30] - 2026-05-20

- fix: suppress huggingface_hub first-download warnings (symlinks, unauth)
- ci: stop submitting to microsoft/winget-pkgs (PR rejected)
- chore: update winget manifests for v0.2.29
- chore: bump nix/package.nix to 0.2.29

## [0.2.29] - 2026-05-19

- feat(release): publish sha256sums.txt; document SHA256 verification + AV false positives
- chore: gitignore hele .claude/-mappen i stedet for kun settings.local.json
- fix: filter Whisper hallucination phrases before injection
- fix(release): rebase before push in nix/package.nix bump step
- chore: auto-bump nix/package.nix version in release workflow
- docs: add CONFIGURATION.md (all settings, values, per-environment examples)
- chore: update winget manifests for v0.2.28

## [0.2.28] - 2026-05-18

- refactor: extract injection/focus into vp_inject.InjectMixin (verbatim)
- refactor: extract audio DSP + arecord probe into vp_audio.py
- refactor: extract _resolve_device/VALID_DEVICES into vp_device.py
- nix: ship X11+Wayland runtime tools on wrapper PATH; drop redundant udev rule
- docs: add MICROPHONE.md explaining [cap]/[gate]/[stt] metrics
- refactor: extract Wayland keymap into vp_keymap.py (behaviour-preserving)
- test: characterization tests for audio DSP (real numpy in CI) before refactor
- fix(wayland): visible dropped-char warning, honest inject log, stronger keycode test

## [0.2.27] - 2026-05-18

- feat: Wayland ydotoold robustness + 5 new keycode layouts; drop Russian
- chore: tighten setup and add tests
- fix: gate quiet raw audio before transcription
- chore: drop official nixpkgs submission; maintain own flake only
- feat(release): auto-bump Homebrew tap on release
- chore: update winget manifests for v0.2.26

## [0.2.26] - 2026-05-17

- feat(installer): uninstall previous version before installing (clean upgrade)
- chore: update winget manifests for v0.2.25

## [0.2.25] - 2026-05-17

- feat(release): version-stamp release asset filenames
- fix(windows): pass launcher args as string[] (was char-split)
- chore: sync nixpkgs/package.nix — linux-only platforms, add maintainer
- chore: update winget manifests for v0.2.24

## [0.2.24] - 2026-05-17

- fix(windows): repair setup.ps1 - ASCII-only + valid subexpression
- chore: sync nixpkgs/package.nix with PR fixes
- chore: gitignore .claude/settings.local.json
- feat: bump winget manifests to schema 1.12.0 with schema headers
- docs: remove broken winget --manifest URL workaround
- chore: update winget manifests for v0.2.23
- fix: use git checkout -f to discard working tree before switching to main
- fix: stash manifests to temp dir before git checkout main
- fix: remove invalid InstallerSuccessCodes and duplicate NVIDIA installer entry
- chore: update winget manifests for v0.2.23
- fix: rename $pid to $productCode in winget manifest step
- chore: add real SRI hash for v0.2.23 nixpkgs derivation
- feat: add Nix flake and nixpkgs derivation

## [0.2.23] - 2026-05-17

- fix: requirements file mapping, diagram alignment, winget admin note
- fix: only build Windows installer when Windows files change; fix diagram alignment
- fix: use choco for Inno Setup install; add wingetcreate auto-PR submission
- fix: repair windows-installer workflow and update Windows install docs

## [0.2.22] - 2026-05-17

- feat: add Inno Setup installer, winget manifests, and Windows CI

## [0.2.21] - 2026-05-17

- feat: per-layout keycode maps for Nordic and German on Wayland

## [0.2.20] - 2026-05-17

- fix: correct DK punctuation injection and add beam_size/initial_prompt tuning

## [0.2.19] - 2026-05-17

- docs: reorganize README by platform with install and start per section

## [0.2.18] - 2026-05-17

- docs: add TECHNICAL.md with architecture diagrams and Wayland details

## [0.2.17] - 2026-05-17

- docs: add supported languages section, fix How it works diagram

## [0.2.16] - 2026-05-17

- docs: remove stale clipboard/VOICEPI_PASTE_KEY references

## [0.2.15] - 2026-05-17

- fix: use numeric evdev keycodes for æøå injection

## [0.2.14] - 2026-05-17

- feat: inject æøå via direct evdev keycodes — no clipboard

## [0.2.13] - 2026-05-17

- fix: wl-copy i baggrunden — undgå deadlock med subprocess.run timeout

## [0.2.12] - 2026-05-16

- fix: brug wl-copy + ydotool key på Wayland — ydotool type kan ikke non-ASCII

## [0.2.11] - 2026-05-16

- fix: dræb gammel ydotoold før restart så dk-layout-env arves korrekt

## [0.2.10] - 2026-05-16

- fix: genstart ydotoold efter GNOME input source-ændring

## [0.2.9] - 2026-05-16

- fix: sæt XKB_DEFAULT_LAYOUT automatisk fra --lang ved startup

## [0.2.8] - 2026-05-16

- fix: sæt XKB_DEFAULT_LAYOUT=dk i ydotoold service + GNOME input source til dk

## [0.2.7] - 2026-05-16

- fix: Wayland injektion via wl-copy + ctrl+shift+v i stedet for ydotool type

## [0.2.6] - 2026-05-16

- fix: fjern da som default language — --lang er nu eksplicit

## [0.2.5] - 2026-05-16

- fix: auto-detektér XKB-layout fra --lang (da→dk, de→de, sv→se…)

## [0.2.4] - 2026-05-16

- fix: inject via ydotool type on Wayland — fjern clipboard+paste, tilføj XKB-layout-detektion

## [0.2.3] - 2026-05-16

- debug: log clipboard set + ydotool result in inject path
- docs: README — brew-baseret Wayland setup via libexec/ubuntu26.04/setup.sh

## [0.2.2] - 2026-05-16

- docs: opdater README — ubuntu26.04/setup.sh som Wayland install-metode
- feat: ubuntu26.04/setup.sh — Wayland+CPU system setup

## [0.2.1] - 2026-05-16

- fix: revert --paste-key default to ctrl+v (universal)
- fix: clipboard + ctrl+shift+v for correct Unicode on Wayland terminals
- fix: use wtype for Wayland text injection (correct Unicode/Danish chars)
- fix: use ydotool type instead of ctrl+v for Wayland injection
- fix: skip xdotool windowactivate for Wayland-native windows
- fix: refocus target window before injection to prevent focus drift
- debug: show focused window on inject, increase settle time to 0.4s
- fix: auto-start ydotoold if socket missing, add daemon setup docs
- fix: use ydotool for text injection on Wayland (replaces pynput XWayland)
- docs: rewrite README with clear Install section and Wayland details

## [0.2.0] - 2026-05-16

- feat: Wayland support — evdev hotkeys, PipeWire audio, chord keys

## [0.1.2] - 2026-05-16

- feat: brew-friendly setup.sh (VOICEPI_PYTHON / VOICEPI_SKIP_SYSCHECK) + Homebrew docs
- ci: bump actions/checkout v4 → v5 (Node 24; v4/Node 20 being removed)

## [0.1.1] - 2026-05-16

- ci: release pipeline — tag push builds 4 bundles + notes from commits
- feat: add double-clickable setup.cmd — true one-click setup on Windows

## [0.1.0] - 2026-05-16

- feat: cross-platform — device auto-detect (CUDA/CPU), turbo default, Linux setup.sh
- feat: voice-dictate — local push-to-talk dictation for Windows
