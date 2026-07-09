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

if [[ -x "${HERE}/whisper-dictate" ]]; then
  SOURCE_BIN="${HERE}/whisper-dictate"
else
  command -v cargo >/dev/null 2>&1 || {
    echo "cargo is required. Install Rust from https://rustup.rs/ and re-run this script." >&2
    exit 1
  }
  # Codex #453 P2 (runtime.rs:634): the v1.20 supervisor requires
  # the full worker-rust feature set (whisper-rs-local + rust-injection
  # + audio-in-rust + rust-hotkeys) because Wave 8 removed the Python
  # fallback. A default-features build would refuse to start dictation
  # with the "missing rust-session feature set" error. Ship the full
  # set from source too.
  cargo build --release -p whisper-dictate-app \
    --manifest-path "${CARGO_MANIFEST}" \
    --target-dir "${HERE}/target" \
    --features "whisper-rs-local,rust-injection,audio-in-rust,rust-hotkeys"
  SOURCE_BIN="${HERE}/target/release/whisper-dictate"
fi

mkdir -p "${BIN_DIR}" "${LIB_DIR}" "${APP_DIR}" "${ICON_DIR}"
install -m 0755 "${SOURCE_BIN}" "${REAL_BIN}"
install -m 0644 "${HERE}/assets/whisper-dictate-logo.svg" "${ICON}"

# Codex #453 P2 (install-rust-ui.sh:37): the `audio-in-rust` feature set
# pulls in `vad-rs` -> `ort`, which dynamically loads the ONNX Runtime
# shared library at runtime. Without `libonnxruntime.so*` next to the
# binary, VAD/audio init fails at first PTT press even though `doctor`
# / `models list` succeed. The release workflow copies the sidecar into
# the tarball for exactly this reason; mirror it for the from-source
# path too. Two search locations, in priority order:
#   1. Next to the pre-built SOURCE_BIN (release bundle case).
#   2. Under the cargo build directory (from-source case).
# Missing files are a soft warning -- an install without the sidecar
# still succeeds (the user might have libonnxruntime.so.* installed
# system-wide via their distro).
_copy_onnxruntime_sidecar_from() {
  local search_dir="$1"
  [ -d "${search_dir}" ] || return 0
  local copied=0
  for lib in "${search_dir}"/libonnxruntime.so*; do
    [ -e "${lib}" ] || continue
    install -m 0755 "${lib}" "${LIB_DIR}/$(basename "${lib}")"
    copied=1
  done
  return "$((1 - copied))"
}
if ! _copy_onnxruntime_sidecar_from "$(dirname "${SOURCE_BIN}")" \
  && ! _copy_onnxruntime_sidecar_from "${HERE}/target/release" \
  && ! _copy_onnxruntime_sidecar_from "${HERE}/target/release/deps"; then
  echo "warning: libonnxruntime.so* not found next to ${SOURCE_BIN} nor under" >&2
  echo "         ${HERE}/target/release/. Rust dictation will require the system" >&2
  echo "         package (Debian/Ubuntu: 'onnxruntime') or a manual copy into" >&2
  echo "         ${LIB_DIR}/ before first PTT press." >&2
fi

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
