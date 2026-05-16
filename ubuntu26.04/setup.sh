#!/usr/bin/env bash
# whisper-dictate — Ubuntu 26.04 LTS (GNOME 50, Wayland) setup
# Idempotent. Run from any directory. Requires sudo for system packages.
#
#   bash ubuntu26.04/setup.sh
#
# What this does:
#   1. Installs whisper-dictate via Homebrew (brew must be installed first)
#   2. Creates gcc-12 symlink needed to build the evdev Python package
#   3. Adds user to the 'input' group (required for evdev hotkeys + ydotool)
#   4. Creates udev rule so /dev/uinput is accessible to the input group
#   5. Installs ydotool (Wayland text injection via kernel uinput)
#   6. Sets up ydotoold as a systemd user service (auto-starts with session)
#   7. Creates an autostart .desktop entry (starts with GNOME login)
set -euo pipefail

STEP=0
step() { STEP=$((STEP+1)); echo; echo "[$STEP] $*"; }
ok()   { echo "    ✓ $*"; }
info() { echo "    → $*"; }
warn() { echo "    ! $*"; }

# ---------------------------------------------------------------------------
step "whisper-dictate: Homebrew-installation"
# ---------------------------------------------------------------------------
if ! command -v brew &>/dev/null; then
    echo "ERROR: Homebrew ikke fundet."
    echo "  Installer: https://brew.sh"
    echo "  Kør derefter: bash ubuntu26.04/setup.sh"
    exit 1
fi

brew tap factusconsulting/tap 2>/dev/null || true
if ! brew list whisper-dictate &>/dev/null 2>&1; then
    info "Installerer whisper-dictate..."
    brew install whisper-dictate
    ok "whisper-dictate installeret"
else
    info "Opdaterer whisper-dictate..."
    brew upgrade whisper-dictate 2>/dev/null && ok "whisper-dictate opdateret" || ok "whisper-dictate er allerede nyeste version"
fi

# ---------------------------------------------------------------------------
step "evdev: gcc-12 symlink (kræves for at bygge evdev Python-pakken)"
# ---------------------------------------------------------------------------
# evdev kompileres med gcc-12, men Ubuntu 26.04 leverer gcc-15.
if [ ! -f /usr/local/bin/gcc-12 ]; then
    GCC=$(command -v gcc-15 || command -v gcc-14 || command -v gcc-13 || true)
    if [ -n "$GCC" ]; then
        sudo ln -sf "$GCC" /usr/local/bin/gcc-12
        ok "gcc-12 → $GCC"
    else
        warn "Ingen gcc fundet — evdev bygges måske ikke korrekt"
    fi
else
    ok "gcc-12 symlink findes allerede"
fi

# ---------------------------------------------------------------------------
step "evdev + ydotool: input-gruppe"
# ---------------------------------------------------------------------------
# evdev kræver input-gruppe for at læse /dev/input/event* (genvejstaster).
# ydotool kræver input-gruppe for at skrive til /dev/uinput (tekstinjektion).
if groups | grep -q '\binput\b'; then
    ok "Bruger er allerede i input-gruppen"
else
    sudo usermod -aG input "$USER"
    ok "Bruger tilføjet til input-gruppen"
    warn "VIGTIGT: Log ud og ind igen for at gruppeskiftet træder i kraft"
fi

# ---------------------------------------------------------------------------
step "ydotool: udev-regel for /dev/uinput"
# ---------------------------------------------------------------------------
UDEV_FILE="/etc/udev/rules.d/60-uinput.rules"
if [ -f "$UDEV_FILE" ] && grep -q 'GROUP="input"' "$UDEV_FILE" 2>/dev/null; then
    ok "udev-regel eksisterer allerede"
else
    echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' | sudo tee "$UDEV_FILE" > /dev/null
    sudo udevadm control --reload-rules && sudo udevadm trigger
    ok "/dev/uinput → input-gruppen"
fi

# ---------------------------------------------------------------------------
step "ydotool: installation"
# ---------------------------------------------------------------------------
if ! command -v ydotool &>/dev/null; then
    sudo apt-get install -y ydotool
    ok "ydotool installeret"
else
    ok "ydotool allerede installeret"
fi

# ---------------------------------------------------------------------------
step "ydotoold: systemd user-service"
# ---------------------------------------------------------------------------
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/ydotoold.service << 'SVCEOF'
[Unit]
Description=ydotool daemon (Wayland input injection)
After=graphical-session.target

[Service]
ExecStart=/usr/bin/ydotoold
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
SVCEOF

systemctl --user daemon-reload
systemctl --user enable --now ydotoold.service 2>/dev/null || true
systemctl --user is-active ydotoold.service &>/dev/null \
    && ok "ydotoold kører" \
    || warn "ydotoold startede ikke — prøv: systemctl --user start ydotoold"

# ---------------------------------------------------------------------------
step "whisper-dictate: autostart ved login"
# ---------------------------------------------------------------------------
# Injektion sker via wl-copy + ctrl+shift+v (terminal paste-genvej).
# Til teksteditorer/browsere: sæt VOICEPI_PASTE_KEY=ctrl+v i autostart-linjen.
mkdir -p "$HOME/.config/autostart"
cat > "$HOME/.config/autostart/whisper-dictate.desktop" << 'EOF'
[Desktop Entry]
Name=Whisper Dictate
Exec=whisper-dictate --key shift_r+ctrl_r --lang da
Icon=audio-input-microphone
Terminal=false
Type=Application
Categories=Utility;
X-GNOME-Autostart-enabled=true
EOF
ok "~/.config/autostart/whisper-dictate.desktop oprettet"

# ---------------------------------------------------------------------------
echo
echo "================================================================"
echo " whisper-dictate Ubuntu 26.04 setup færdig"
echo "================================================================"
echo
if ! groups | grep -q '\binput\b'; then
    echo "  NÆSTE SKRIDT: Log ud og ind igen (input-gruppe aktiveres)"
    echo
    echo "  Kør derefter første gang for at downloade Whisper-modellen:"
    echo "  whisper-dictate --key shift_r+ctrl_r --lang da"
else
    echo "  Test: hold højre Shift+Ctrl, tal, slip"
    echo "  Teksten indsættes i det vindue der havde fokus da du trykkede."
    echo
    echo "  Kør manuelt (starter også ved næste login automatisk):"
    echo "  whisper-dictate --key shift_r+ctrl_r --lang da"
    echo
    echo "  Til teksteditorer/browsere (ctrl+v i stedet for ctrl+shift+v):"
    echo "  VOICEPI_PASTE_KEY=ctrl+v whisper-dictate --key shift_r+ctrl_r --lang da"
fi
echo
