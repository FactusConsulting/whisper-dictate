# Changelog

All notable changes to whisper-dictate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Entries below v0.2.36 were generated retroactively from `git log` per
tag; chore-bump lines (winget manifests, nix/package.nix) trail each
release because `release.yml` bumps in-place after the tag is pushed.

## [Unreleased]

### Added
- `--model-capacity` and Settings UI model-fit check for local GPU VRAM and Whisper/Parakeet/Ollama model guidance.
- Optional OpenAI-compatible STT backend via `VOICEPI_STT_BACKEND=openai`, `VOICEPI_STT_MODEL`, `VOICEPI_STT_BASE_URL`, `VOICEPI_STT_TIMEOUT_MS`, and `VOICEPI_STT_API_KEY`/`OPENAI_API_KEY`.
- Optional OpenAI-compatible post-processing provider via `VOICEPI_POST_PROCESSOR=openai` and `VOICEPI_POST_API_KEY`/`OPENAI_API_KEY`.
- Settings UI controls and benchmark specs for external STT models, including `openai:gpt-4o-mini-transcribe`.
- Rust UI Groq cloud STT preset, Groq API key shortcut, and `GROQ_API_KEY` support when using Groq's OpenAI-compatible endpoint.
- The Windows PowerShell title, Settings UI title and tray tooltip now show the running whisper-dictate version.
- The official whisper-dictate logo is used as the Windows application icon and shown on the GitHub README/release notes.

### Changed
- Rust Windows UI now uses the Windows GUI subsystem so normal UI launch does not create an extra console window.
- Rust UI-managed worker, doctor and install commands now hide child console windows on Windows while streaming logs into the Runtime tab.
- Rust Runtime log now expands to the available window width/height instead of staying as a narrow text box.
- Rust Runtime tab now has a Clear button for clearing the visible log without stopping dictation.
- Rust Runtime Stop now returns immediately and tears down the Python worker process tree in the background.
- External API keys are read from environment variables only and are not stored in the Settings UI config JSON.

## [0.2.60] - 2026-05-31

### Added
- `VOICEPI_POST_MODE=bullet-list` alias for the existing `bullets` post-processing mode, matching the roadmap wording.

### Changed
- Dictionary replacement suggestions now filter common words, known source terms and noisy benchmark fragments more aggressively.

## [0.2.59] - 2026-05-31

### Added
- `VOICEPI_FORMAT_COMMANDS` opt-in spoken formatting commands for deterministic punctuation and line breaks.
- Settings UI support for enabling English, Danish or combined formatting command sets.
- `--dictionary-suggest JSONL` to propose smart replacements from benchmark/history output without mutating the dictionary.
- Settings UI dictionary suggestion preview/apply flow for benchmark/history JSONL.

## [0.2.58] - 2026-05-30

### Added
- `VOICEPI_COMMAND_HOOK` advanced automation hook for running an opt-in command after each accepted utterance.
- `VOICEPI_COMMAND_HOOK_TIMEOUT_MS` to bound hook latency; hook failures/timeouts are logged and recorded without blocking injection.
- Settings UI and documentation for the command hook.

## [0.2.57] - 2026-05-30

### Added
- Config-driven target profiles matched by active window title/process.
- Profiles can override live-safe settings such as `inject_mode`, `lang`, `initial_prompt`, dictionary settings and audio thresholds for matched targets.
- Active profile is logged and included in metrics/history events.

## [0.2.56] - 2026-05-30

### Added
- Local dictation history JSONL storage for accepted live utterances.
- `VOICEPI_HISTORY_ENABLED` and `VOICEPI_HISTORY_JSONL` settings for local history control/location.
- History CLI helpers: `--history-list`, `--history-last`, `--history-copy-last`, and `--history-reinject-last`.

## [0.2.55] - 2026-05-30

### Added
- `--calibrate-mic [SECONDS]` records a short microphone sample and recommends audio threshold settings.
- `--calibrate-file PATH` analyzes an existing audio file for repeatable calibration/testing.
- Calibration JSON output via `--json`, including raw dBFS, noise floor, SNR, peak and recommended `VOICEPI_TARGET_DBFS`, `VOICEPI_MIN_INPUT_DBFS`, and `VOICEPI_MIN_SNR_DB`.

## [0.2.54] - 2026-05-30

### Added
- `--benchmark-files` command to run one or more audio files through selected backend/model specs.
- `--benchmark-backends` accepts comma-separated specs such as `whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3`.
- `--benchmark-jsonl` writes one structured result per file/backend, including success/failure metadata.

## [0.2.53] - 2026-05-30

### Added
- `--transcribe-file PATH` command for reproducible file transcription using the selected backend/config.
- Native 16-bit WAV decoding with mono conversion and resampling to 16 kHz.
- ffmpeg fallback for mp3/m4a/other formats when ffmpeg is installed.
- File transcription JSON output using `--json`, including backend/model, timing, language, source file and dictionary replacement metadata.

## [0.2.52] - 2026-05-30

### Added
- Windows Settings UI now has visible `?` help on Core, Quality, Dictionary and Output settings. The help opens on click and is shown explicitly on hover so it does not depend on platform tooltip timing.
- `VOICEPI_RELEASE_TAIL_MS` live setting adds a short audio tail after hotkey release to avoid clipping final syllables/words.
- Parakeet startup/transcription noise from NeMo is hidden by default and exposed only with `VOICEPI_STT_DEBUG=1`.

### Changed
- Parakeet model choices are trimmed to the practical options: multilingual `nvidia/parakeet-tdt-0.6b-v3`, pure-English quality `nvidia/parakeet-tdt-1.1b`, and fast English-only `nvidia/parakeet-tdt-0.6b-v2`.
- Settings UI disables Whisper-only controls, including compute type, when Parakeet is selected.
- Documentation now clarifies that Parakeet v3 autodetects language and does not support forcing `VOICEPI_LANG=da`.

### Fixed
- Windows injection now uses paste automatically for layout-sensitive punctuation such as English contractions on Danish keyboard layouts.
- Parakeet/NeMo progress and training-related logs no longer clutter the normal UI runtime log.

## [0.2.41] - 2026-05-23

### Added
- `VOICEPI_INJECT_MODE=auto` is now the default injection strategy. It types directly except for known fragile Windows terminal targets, where it uses clipboard paste.
- `--type` CLI flag to force direct keyboard typing when `auto` or env configuration would choose another strategy.
- `scripts/inject-smoke.py` for manual injection checks against Notepad, Windows Terminal, Claude Code, browser fields, and other targets without loading Whisper.
- Startup now prints the running `whisper-dictate` version in the launcher/terminal window. Release zips and Windows installers get the version from the tag; development checkouts fall back to `git describe`.

### Changed
- `pynput` is lazy-loaded again so `python voice_pi.py --help` stays independent of OS keyboard backends.
- Replaced the pynput listener polling loop with `Listener.join()` and changed the `arecord` probe from raw sleep to process timeout handling.
- `windows-installer.yml` now treats `vp_*.py` changes as Windows-relevant, so installer builds are not skipped when split modules change.
- Windows/Linux launchers no longer force `--paste`; they let the app's `auto` injection strategy decide unless the user passes an explicit mode.

## [0.2.40] - 2026-05-23

### Removed
- Removed the `VOICEPI_TYPE_INTERVAL_MS` per-character typing delay and restored direct `pynput.Controller.type(text)` injection.

### Changed
- Removed fixed sleeps around text injection and replaced ydotoold startup waits with short readiness polling.
- Added a permissive repo-local Codex configuration for trusted local development.

## [0.2.38] - 2026-05-23

### Fixed
- **Injection regression on Windows**: revert `from pynput import keyboard` from lazy (inside `Dictate.__init__`) back to module top-level in `voice_pi.py` and `vp_inject.py`. The lazy import (introduced in v0.2.36's refactor) appears to have introduced a subtle timing issue where pynput's `Controller.type(text)` dropped some spaces between words on the target terminal — reported as concatenations like "harjegbundetden" / "stillsomethingfishy" in injected text even though `[stt]` showed the correct Whisper output. `sounddevice` stays lazy so the smoke-test job remains lightweight.

## [0.2.37] - 2026-05-23

### Added
- `VOICEPI_TEMPERATURE` env-var: comma-separated list of Whisper decode temperatures (default `0.0,0.2`). Set to `0.0` to lock to greedy decode and eliminate the fallback that produces "creative" output on uncertain segments.
- `VOICEPI_CONTEXT_MIN_SECONDS` env-var: pass `condition_on_previous_text=True` only when the utterance is at least this long (default `0` = always off). Useful for long sentences where context helps Whisper keep word boundaries coherent.
- 6 new unit tests covering `_parse_temperatures` parsing and the duration-gated context decision.

## [0.2.36] - 2026-05-23

### Added
- `CHANGELOG.md` (Keep-a-Changelog) covering every release from v0.1.0 onward.
- `vp_cli.py` (argparse + `VOICEPI_DEBUG` settings dump) and `vp_transcribe.py` (Whisper call + hallucination filter) — extracted from `voice_pi.py` so each module has a single responsibility. `voice_pi.py` re-exports every name so installer/tests/downstream callers are unaffected.
- Cross-platform `smoke` CI job (ubuntu-latest + windows-latest) running `voice_pi.py --help` and `scripts/probe-key.py bogus_key 1`.
- `lint-workflows` CI job: `yaml.safe_load` of every workflow + `rhysd/actionlint`.
- 15 new unit tests: `ModuleSurfaceTests`, `HallucinationFilterTests`, `CliModuleIsolationTests` (48 → 63 total).

### Changed
- `voice_pi.py` shrunk from 577 to 401 lines after the split.
- `sounddevice`, `pynput`, and `faster_whisper` are now lazy-imported inside `Dictate` methods and `__main__`. `python voice_pi.py --help` runs with only `numpy` installed.
- Soft-pinned `sounddevice>=0.4,<0.6`, `pynput>=1.7,<2.0`, `pyperclip>=1.8,<2.0`, `evdev>=1.6,<2.0` in both `requirements-cpu.txt` and `requirements-gpu.txt`.
- `release.yml` builds the release asset list as a bash array (shellcheck SC2086).

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
