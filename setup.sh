#!/usr/bin/env bash
# =====================================================================
# whisper-dictate — one-shot setup + launcher (Linux, CPU).
#
# Copy the whole folder to an Ubuntu box and run:  ./setup.sh
# Idempotent: first run builds a machine-local venv + installs deps +
# downloads the model + launches; later runs just launch. The venv is
# NOT inside this folder, so copying the folder never drags a broken
# venv along. voice_pi.py auto-detects CUDA vs CPU; with no NVIDIA GPU
# it runs CPU (slower — that's why the model default is the fastest,
# large-v3-turbo).
#
# Args pass straight to voice_pi.py, e.g.:  ./setup.sh --lang de
# With none it defaults to: --paste --key shift_r+ctrl_r  (Wayland)
#                        or: --paste                       (X11)
# Stop the running tool with Esc (or Ctrl+C).
#
# WAYLAND SETUP (one-time, Ubuntu 24.04+):
#   Global hotkeys work via evdev — reading /dev/input/event* directly.
#   This requires:
#     1. sudo usermod -aG input $USER    (then log out and back in)
#     2. First run rebuilds the venv with evdev + scipy
#   After that, hold right Shift + right Ctrl to talk (default chord).
#   Audio is captured via arecord -D pipewire, bypassing PortAudio's
#   direct ALSA open which misses PipeWire's virtual mic routing.
# =====================================================================
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV="${HOME}/.venv-whisper-dictate"
VENVPY="${VENV}/bin/python"
APP="${HERE}/voice_pi.py"
# Python to build the venv from. Override for non-apt environments
# (e.g. the Homebrew formula points this at the brewed python@3.12).
PYBIN="${VOICEPI_PYTHON:-python3}"

# Requirements: bundle's requirements.txt wins; else the CPU file
# (Linux default); else the GPU file.
REQ=""
for f in "${HERE}/requirements.txt" "${HERE}/requirements-cpu.txt" "${HERE}/requirements-gpu.txt"; do
  [ -f "$f" ] && { REQ="$f"; break; }
done
[ -n "$REQ" ] || { echo "no requirements file next to setup.sh" >&2; exit 1; }

# Default args: on Wayland use chord hotkey; on X11 use single key.
if [ "$#" -gt 0 ]; then
  ARGS=("$@")
elif [ "${WAYLAND_DISPLAY:-}" != "" ] || [ "${XDG_SESSION_TYPE:-}" = "wayland" ]; then
  ARGS=(--paste --key shift_r+ctrl_r)
else
  ARGS=(--paste)
fi

# --- system prerequisites ------------------------------------------
# Skipped when VOICEPI_SKIP_SYSCHECK is set (the Homebrew formula sets
# it — brew already guarantees python@3.12 + portaudio via deps).
"$PYBIN" -c 'import sys; raise SystemExit(0 if sys.version_info[:2] >= (3,10) else 1)' \
  || { echo "Need Python >= 3.10 ($PYBIN --version: $("$PYBIN" --version 2>&1))" >&2; exit 1; }
if [ -z "${VOICEPI_SKIP_SYSCHECK:-}" ]; then
  need_apt=()
  "$PYBIN" -m venv --help >/dev/null 2>&1 || need_apt+=("python3-venv")
  ldconfig -p 2>/dev/null | grep -q libportaudio || need_apt+=("libportaudio2")
  command -v arecord >/dev/null 2>&1 || need_apt+=("alsa-utils")
  if [ "${#need_apt[@]}" -gt 0 ]; then
    echo "Missing system packages. Run this once, then re-run setup.sh :" >&2
    echo "    sudo apt update && sudo apt install -y ${need_apt[*]}" >&2
    exit 1
  fi
fi

# Clipboard tool for --paste (Wayland → wl-clipboard, X11 → xclip).
if [ "${XDG_SESSION_TYPE:-}" = "wayland" ] || [ "${WAYLAND_DISPLAY:-}" != "" ]; then
  command -v wl-copy >/dev/null 2>&1 || \
    echo "WARNING: no wl-copy (install wl-clipboard) — --paste needs it" >&2
else
  command -v xclip >/dev/null 2>&1 || command -v xsel >/dev/null 2>&1 || \
    echo "WARNING: no xclip/xsel — --paste needs a clipboard tool" >&2
fi

# Wayland: check input group for evdev hotkey access
if [ "${WAYLAND_DISPLAY:-}" != "" ] || [ "${XDG_SESSION_TYPE:-}" = "wayland" ]; then
  if ! groups | grep -q '\binput\b'; then
    echo "WARNING: not in the 'input' group — global hotkeys won't work on Wayland." >&2
    echo "         Fix: sudo usermod -aG input \$USER  (then log out and back in)" >&2
  fi
fi

# --- fast path: venv that can already import the engine -------------
if "$VENVPY" -c 'import faster_whisper, numpy, sounddevice, pynput' >/dev/null 2>&1; then
  :
else
  echo "Setting up whisper-dictate (one-time on this machine)..."
  rm -rf "$VENV"
  "$PYBIN" -m venv "$VENV"
  "$VENVPY" -m pip install --upgrade pip
  "$VENVPY" -m pip install -r "$REQ"
  echo "Setup complete."
fi

echo "Starting whisper-dictate — press Esc (or Ctrl+C) to stop."
cd "$HERE"
exec "$VENVPY" "$APP" "${ARGS[@]}"
