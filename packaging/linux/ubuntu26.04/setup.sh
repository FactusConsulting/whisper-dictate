#!/usr/bin/env bash
# whisper-dictate — Ubuntu 26.04 LTS (GNOME 50, Wayland) setup
# Idempotent. Run from any directory. Requires sudo for system packages.
#
#   bash packaging/linux/ubuntu26.04/setup.sh
#
# Environment variables (optional — defaults shown):
#   WD_LANG   spoken-language code for whisper-dictate (default: da)
#   WD_XKB    XKB keyboard layout for ydotoold / GNOME input source
#             (default: derived from WD_LANG — da→dk, otherwise WD_LANG)
#
# What this does:
#   1. Installs whisper-dictate via Homebrew (brew must be installed first)
#   2. Adds user to the 'input' group (required for evdev/rdev hotkeys + ydotool)
#   3. Creates udev rule so /dev/uinput is accessible to the input group
#   4. Installs ydotool (Wayland text injection via kernel uinput)
#   5. Sets up ydotoold as a systemd user service (auto-starts with session)
#   6. When run directly, creates GNOME launcher/autostart entries and starts the UI
#      (when run by `whisper-dictate setup-ubuntu`, Rust owns this final step)
set -euo pipefail

# ---------------------------------------------------------------------------
# Language / keyboard layout configuration
# ---------------------------------------------------------------------------
WD_LANG="${WD_LANG:-da}"
# Derive XKB layout from language: da → dk, otherwise use the language code.
if [[ -z "${WD_XKB:-}" ]]; then
    if [[ "$WD_LANG" == "da" ]]; then
        WD_XKB="dk"
    else
        WD_XKB="$WD_LANG"
    fi
fi

STEP=0
step() { STEP=$((STEP+1)); echo; echo "[$STEP] $*"; }
ok()   { echo "    ✓ $*"; }
info() { echo "    → $*"; }
warn() { echo "    ! $*"; }
SEP="================================================================"

# ---------------------------------------------------------------------------
step "whisper-dictate: Homebrew-installation"
# ---------------------------------------------------------------------------
if ! command -v brew &>/dev/null; then
    echo "ERROR: Homebrew ikke fundet." >&2
    echo "  Installer: https://brew.sh" >&2
    echo "  Kør derefter: bash packaging/linux/ubuntu26.04/setup.sh" >&2
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
step "GNOME: tastaturlayout $WD_XKB"
# ---------------------------------------------------------------------------
# GNOME bruger "us"-layout for uinput-enheder (whisper-dictates virtuelle tastatur)
# selv om det fysiske tastatur virker korrekt. Tilføj WD_XKB-layout til input
# sources (bevarer eksisterende layouts) så compositor fortolker KEY_LEFTBRACE
# → å i stedet for [.
current_sources=$(gsettings get org.gnome.desktop.input-sources sources 2>/dev/null || echo "")
if echo "$current_sources" | grep -q "'${WD_XKB}'"; then
    ok "GNOME input source indeholder allerede $WD_XKB"
else
    # Append the wanted layout rather than overwriting the whole list.
    # gsettings returns either "@a(ss) []" (empty) or "[('xkb', 'a'), ...]";
    # an empty string means the gsettings call itself failed — treat both as
    # "no existing layouts" so we never build an invalid ", (...)]" list.
    if [[ -z "$current_sources" ]] || echo "$current_sources" | grep -q '@a(ss) \[\]'; then
        # Empty list — start fresh with just our layout.
        new_sources="[('xkb', '${WD_XKB}')]"
    else
        # Strip the trailing "]" and append our entry.
        new_sources="${current_sources%]}, ('xkb', '${WD_XKB}')]"
    fi
    gsettings set org.gnome.desktop.input-sources sources "$new_sources"
    ok "GNOME input source $WD_XKB tilføjet (påkrævet for specialtegn via ydotool type)"
fi

# ---------------------------------------------------------------------------
step "ydotool: udev-regel for /dev/uinput"
# ---------------------------------------------------------------------------
UDEV_FILE="/etc/udev/rules.d/60-uinput.rules"
if [[ -f "$UDEV_FILE" ]] && grep -q 'GROUP="input"' "$UDEV_FILE" 2>/dev/null; then
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
# XKB_DEFAULT_LAYOUT i daemonen er afgørende: det er ydotoold der
# konverterer tegn (æøå) til keycodes — klient-processens env har ingen effekt.
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/ydotoold.service << SVCEOF
[Unit]
Description=ydotool daemon (Wayland input injection)
After=graphical-session.target

[Service]
ExecStart=/usr/bin/ydotoold
Environment=XKB_DEFAULT_LAYOUT=${WD_XKB}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
SVCEOF

systemctl --user daemon-reload
systemctl --user enable ydotoold.service 2>/dev/null || true
# Stop service og dræb evt. kørende ydotoold-proces så en gammel daemon
# ikke blokerer socketen og forhindrer systemd i at starte ny.
systemctl --user stop ydotoold.service 2>/dev/null || true
systemctl --user reset-failed ydotoold.service 2>/dev/null || true
pkill -x ydotoold 2>/dev/null || true
for _ in 1 2 3 4 5; do
    pgrep -x ydotoold >/dev/null 2>&1 || break
    sleep 0.2
done
if pgrep -x ydotoold >/dev/null 2>&1; then
    pkill -KILL -x ydotoold 2>/dev/null || true
    sleep 0.2
fi
YDOTOOL_SOCKET_PATH="${YDOTOOL_SOCKET:-${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/.ydotool_socket}"
if [[ -S "$YDOTOOL_SOCKET_PATH" ]] && ! pgrep -x ydotoold >/dev/null 2>&1; then
    rm -f "$YDOTOOL_SOCKET_PATH"
fi
systemctl --user restart ydotoold.service 2>/dev/null || systemctl --user start ydotoold.service 2>/dev/null || true
sleep 1
if systemctl --user is-active ydotoold.service &>/dev/null; then
    ok "ydotoold kører (XKB_DEFAULT_LAYOUT=${WD_XKB})"
elif pgrep -x ydotoold &>/dev/null; then
    ok "ydotoold kører (manuel start)"
else
    warn "ydotoold startede ikke — prøv: systemctl --user start ydotoold"
fi

# ---------------------------------------------------------------------------
step "whisper-dictate: GNOME app launcher"
# ---------------------------------------------------------------------------
if [[ "${VOICEPI_RUST_OWNS_DESKTOP:-}" = "1" ]]; then
    ok "Rust CLI handles launcher/autostart creation and UI startup"
    echo
    echo "$SEP"
    echo " whisper-dictate Ubuntu 26.04 system setup færdig"
    echo "$SEP"
    echo
    if ! groups | grep -q '\binput\b'; then
        echo "  NÆSTE SKRIDT: Log ud og ind igen (input-gruppe aktiveres)"
        echo
    fi
    exit 0
fi

mkdir -p "$HOME/.local/share/applications" "$HOME/.config/autostart"
cat > "$HOME/.local/share/applications/whisper-dictate.desktop" << 'EOF'
[Desktop Entry]
Name=Whisper Dictate
Comment=Push-to-talk dictation settings and runtime control
Exec=whisper-dictate ui
Icon=whisper-dictate
Terminal=false
Type=Application
Categories=Utility;AudioVideo;Audio;
StartupNotify=true
StartupWMClass=whisper-dictate
EOF
chmod 0644 "$HOME/.local/share/applications/whisper-dictate.desktop"
ok "~/.local/share/applications/whisper-dictate.desktop oprettet"

if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
fi

cp "$HOME/.local/share/applications/whisper-dictate.desktop" \
   "$HOME/.config/autostart/whisper-dictate.desktop"
cat >> "$HOME/.config/autostart/whisper-dictate.desktop" << 'EOF'
X-GNOME-Autostart-enabled=true
EOF
ok "~/.config/autostart/whisper-dictate.desktop oprettet (starter UI ved login)"

# ---------------------------------------------------------------------------
step "whisper-dictate: start UI"
# ---------------------------------------------------------------------------
if command -v gtk-launch &>/dev/null; then
    gtk-launch whisper-dictate >/dev/null 2>&1 &
    ok "Whisper Dictate UI startes via app launcher"
elif command -v setsid &>/dev/null; then
    setsid whisper-dictate ui >/dev/null 2>&1 &
    ok "Whisper Dictate UI startes"
else
    whisper-dictate ui >/dev/null 2>&1 &
    ok "Whisper Dictate UI startes"
fi

# ---------------------------------------------------------------------------
echo
echo "$SEP"
echo " whisper-dictate Ubuntu 26.04 setup færdig"
echo "$SEP"
echo
if ! groups | grep -q '\binput\b'; then
    echo "  NÆSTE SKRIDT: Log ud og ind igen (input-gruppe aktiveres)"
    echo
    echo "  Åbn derefter appen fra Ubuntu launcher: Whisper Dictate"
    echo "  Eller kør: whisper-dictate ui"
else
    echo "  UI'et burde åbne nu. Tryk Start i Runtime-fanen."
    echo "  Test: hold højre Shift+Ctrl, tal, slip."
    echo "  Teksten indsættes i det vindue der havde fokus da du trykkede."
    echo
    echo "  Start manuelt: whisper-dictate ui"
    echo "  Terminal-runtime: whisper-dictate run --key shift_r+ctrl_r --lang ${WD_LANG}"
fi
echo
