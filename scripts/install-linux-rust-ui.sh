#!/usr/bin/env bash
# Build and install the Rust desktop UI/controller for the current user.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
BIN="${BIN_DIR}/whisper-dictate"
DESKTOP="${APP_DIR}/whisper-dictate.desktop"

command -v cargo >/dev/null 2>&1 || {
  echo "cargo is required. Install Rust from https://rustup.rs/ and re-run this script." >&2
  exit 1
}

cargo build --release -p whisper-dictate-app --manifest-path "${HERE}/Cargo.toml"

mkdir -p "${BIN_DIR}" "${APP_DIR}"
install -m 0755 "${HERE}/target/release/whisper-dictate" "${BIN}"

cat > "${DESKTOP}" <<EOF
[Desktop Entry]
Type=Application
Name=whisper-dictate
Comment=Push-to-talk dictation settings and runtime control
Exec=${BIN} ui
Terminal=false
Categories=Utility;AudioVideo;Audio;
StartupNotify=true
EOF

chmod 0644 "${DESKTOP}"

echo "Installed ${BIN}"
echo "Installed ${DESKTOP}"
echo "Run: whisper-dictate ui"
