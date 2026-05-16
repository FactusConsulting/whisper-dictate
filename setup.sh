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
# With none it defaults to: --paste   (model default = large-v3-turbo).
# Stop the running tool with Esc (or Ctrl+C).
#
# WAYLAND CAVEAT (read this): global hotkey capture and synthetic
# keystroke injection (pynput) are designed for X11. On GNOME/Wayland
# the compositor blocks both for unprivileged apps, so push-to-talk
# and auto-typing may NOT work out of the box. Realistic options:
#   * log in choosing "Ubuntu on Xorg" at the login screen, OR
#   * use --no-type to just see the transcription (pipeline proof), OR
#   * the proper Wayland path (evdev hotkey + ydotool injection, needs
#     one-time `input`-group / uinput permissions) — see README.
# The venv/model/transcription themselves work fine regardless.
# =====================================================================
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV="${HOME}/.venv-whisper-dictate"
VENVPY="${VENV}/bin/python"
APP="${HERE}/voice_pi.py"

# Requirements: bundle's requirements.txt wins; else the CPU file
# (Linux default); else the GPU file.
REQ=""
for f in "${HERE}/requirements.txt" "${HERE}/requirements-cpu.txt" "${HERE}/requirements-gpu.txt"; do
  [ -f "$f" ] && { REQ="$f"; break; }
done
[ -n "$REQ" ] || { echo "no requirements file next to setup.sh" >&2; exit 1; }

# Default args if none given (turbo is the model default in voice_pi.py).
if [ "$#" -gt 0 ]; then ARGS=("$@"); else ARGS=(--paste); fi

# --- system prerequisites we cannot install without sudo ------------
need_apt=()
python3 -c 'import sys; raise SystemExit(0 if sys.version_info[:2] >= (3,10) else 1)' \
  || { echo "Need Python >= 3.10 (python3 --version: $(python3 --version 2>&1))" >&2; exit 1; }
python3 -m venv --help >/dev/null 2>&1 || need_apt+=("python3-venv")
# PortAudio runtime for sounddevice:
ldconfig -p 2>/dev/null | grep -q libportaudio || need_apt+=("libportaudio2")
# Clipboard tool for --paste (Wayland → wl-clipboard, X11 → xclip):
if [ "${XDG_SESSION_TYPE:-}" = "wayland" ]; then
  command -v wl-copy >/dev/null 2>&1 || need_apt+=("wl-clipboard")
else
  command -v xclip >/dev/null 2>&1 || command -v xsel >/dev/null 2>&1 || need_apt+=("xclip")
fi
if [ "${#need_apt[@]}" -gt 0 ]; then
  echo "Missing system packages. Run this once, then re-run ./setup.sh :" >&2
  echo "    sudo apt update && sudo apt install -y ${need_apt[*]}" >&2
  exit 1
fi

# --- fast path: venv that can already import the engine -------------
if "$VENVPY" -c 'import faster_whisper, numpy, sounddevice, pynput' >/dev/null 2>&1; then
  :
else
  echo "Setting up whisper-dictate (one-time on this machine)..."
  rm -rf "$VENV"
  python3 -m venv "$VENV"
  "$VENVPY" -m pip install --upgrade pip
  "$VENVPY" -m pip install -r "$REQ"
  echo "Setup complete."
fi

if [ "${XDG_SESSION_TYPE:-}" = "wayland" ]; then
  echo "WARNING: Wayland session detected — global hotkey / auto-typing"
  echo "         may not work (see the WAYLAND CAVEAT in this script)."
fi
echo "Starting whisper-dictate — press Esc (or Ctrl+C) to stop."
cd "$HERE"
exec "$VENVPY" "$APP" "${ARGS[@]}"
