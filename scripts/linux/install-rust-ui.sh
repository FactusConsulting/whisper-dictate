#!/usr/bin/env bash
# Build and install the Rust desktop UI/controller for the current user.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -f "${SCRIPT_DIR}/../../src/rust/Cargo.toml" && -d "${SCRIPT_DIR}/../../src/rust" ]]; then
  HERE="$(cd "${SCRIPT_DIR}/../.." && pwd)"
else
  HERE="$(cd "${SCRIPT_DIR}/.." && pwd)"
fi
CARGO_MANIFEST="${HERE}/src/rust/Cargo.toml"
BIN_DIR="${HOME}/.local/bin"
LIB_DIR="${HOME}/.local/lib/whisper-dictate"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"
BIN="${BIN_DIR}/whisper-dictate"
REAL_BIN="${LIB_DIR}/whisper-dictate-app"
DESKTOP="${APP_DIR}/whisper-dictate.desktop"
ICON="${ICON_DIR}/whisper-dictate.svg"

require_source_build_prerequisites() {
  local missing=()
  local command_name
  for command_name in cargo cc c++ pkg-config cmake clang; do
    command -v "${command_name}" >/dev/null 2>&1 || missing+=("${command_name}")
  done

  local module
  for module in alsa dbus-1 wayland-client x11 xi xtst xkbcommon xcb-render xcb-shape xcb-xfixes; do
    pkg-config --exists "${module}" 2>/dev/null || missing+=("pkg-config:${module}")
  done

  if command -v dpkg-query >/dev/null 2>&1 &&
     ! dpkg-query -W -f='${Status}' libclang-dev 2>/dev/null | grep -Fq "install ok installed"; then
    missing+=("libclang-dev")
  fi

  if ((${#missing[@]})); then
    printf 'Native source-build prerequisites are missing: %s\n' "${missing[*]}" >&2
    echo "On Ubuntu/Debian install the packages listed in docs/INSTALLATION.md, then re-run this script." >&2
    exit 1
  fi
}

if [[ -x "${HERE}/whisper-dictate" ]]; then
  SOURCE_BIN="${HERE}/whisper-dictate"
else
  require_source_build_prerequisites
  # The legacy fallback has retired. Build the complete native dictation route so
  # the installed UI and `whisper-dictate run` can capture, transcribe, handle
  # the global PTT chord, and inject text.
  cargo build --release -p whisper-dictate-app --features rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local --manifest-path "${CARGO_MANIFEST}" --target-dir "${HERE}/target"
  SOURCE_BIN="${HERE}/target/release/whisper-dictate"
fi

mkdir -p "${BIN_DIR}" "${LIB_DIR}" "${APP_DIR}" "${ICON_DIR}"
install -m 0755 "${SOURCE_BIN}" "${REAL_BIN}"
# `audio-in-rust` pulls in ort, whose copy-dylibs feature puts the system
# ONNX Runtime shared objects beside the built/prepackaged executable. Keep
# them beside the relocated executable too or startup fails before diagnostics
# can initialise.
ONNX_COUNT=0
while IFS= read -r onnx_lib; do
  install -m 0644 "${onnx_lib}" "${LIB_DIR}/$(basename "${onnx_lib}")"
  ONNX_COUNT=$((ONNX_COUNT + 1))
done < <(find "$(dirname "${SOURCE_BIN}")" -maxdepth 1 -name 'libonnxruntime.so*' -print)
if ((ONNX_COUNT == 0)); then
  echo "Native install failed: libonnxruntime.so* was not produced beside ${SOURCE_BIN}." >&2
  echo "Rebuild with the complete audio-in-rust feature set before installing." >&2
  exit 1
fi
echo "Installed ${ONNX_COUNT} ONNX Runtime shared object(s) in ${LIB_DIR}"
install -m 0644 "${HERE}/assets/whisper-dictate-logo.svg" "${ICON}"

cat > "${BIN}" <<EOF
#!/usr/bin/env bash
export VOICEPI_APP_ROOT="${HERE}"
exec "${REAL_BIN}" "\$@"
EOF
chmod 0755 "${BIN}"

cat > "${DESKTOP}" <<EOF
[Desktop Entry]
Type=Application
Name=Whisper Dictate
Comment=Push-to-talk dictation settings and runtime control
Exec=${BIN} ui
Icon=${ICON}
Terminal=false
Categories=Utility;AudioVideo;Audio;
StartupNotify=true
StartupWMClass=whisper-dictate
EOF

chmod 0644 "${DESKTOP}"
gtk-update-icon-cache -q "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true

ensure_user_bin_first() {
  local profile="$1"
  if [[ -f "${profile}" ]] && grep -Fq 'export PATH="${HOME}/.local/bin:${PATH}"' "${profile}"; then
    return
  fi
  {
    echo
    echo "# whisper-dictate user install"
    echo 'export PATH="${HOME}/.local/bin:${PATH}"'
  } >> "${profile}"
}

if [[ "$(command -v whisper-dictate 2>/dev/null || true)" != "${BIN}" ]]; then
  ensure_user_bin_first "${HOME}/.profile"
  if [[ "${SHELL:-}" = */zsh ]] || [[ -f "${HOME}/.zprofile" ]]; then
    ensure_user_bin_first "${HOME}/.zprofile"
  fi
fi

echo "Installed ${BIN}"
echo "Installed ${REAL_BIN}"
echo "Installed ${DESKTOP}"
echo "Installed ${ICON}"
if [[ "$(command -v whisper-dictate 2>/dev/null || true)" = "${BIN}" ]]; then
  echo "Run: whisper-dictate ui"
else
  echo "Run now: ${BIN} ui"
  echo "Open a new shell to use: whisper-dictate ui"
fi
