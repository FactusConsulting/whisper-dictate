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
  # Build with `--features audio-capture` so the installed binary carries
  # the native cpal recorder that shipping releases have. Without this flag
  # cargo picks up the crate's `default = ["ui-egui-glow"]` set only, and
  # `corpus-record` compiles to the "rebuild with --features audio-capture"
  # stub in `src/rust/corpus_record.rs` (added in PR #629 when the Python
  # `vp_corpus_record.py` fallback was retired). Source installs therefore
  # end up with a UI that offers Record but a CLI that refuses to record.
  # The release binary at .github/workflows/release.yml:123 has a larger
  # feature set (`rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local`)
  # but those pull in heavy build deps (ONNX runtime, cmake+clang for
  # whisper.cpp) that a source install on a fresh box will not have; the
  # minimum needed to close #629 is `audio-capture` alone.
  cargo build --release -p whisper-dictate-app --features audio-capture --manifest-path "${CARGO_MANIFEST}" --target-dir "${HERE}/target"
  SOURCE_BIN="${HERE}/target/release/whisper-dictate"
fi

mkdir -p "${BIN_DIR}" "${LIB_DIR}" "${APP_DIR}" "${ICON_DIR}"
install -m 0755 "${SOURCE_BIN}" "${REAL_BIN}"
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
