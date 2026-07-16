#!/usr/bin/env bash
# Canonical Wayland user smoke — run on the Ubuntu 26.04 Wayland box after
# installing a new whisper-dictate release. Verifies, headless, that every
# shipped user-facing feature still works. Exits 0 on all-pass, non-zero on
# any fail.
#
# See docs/dev/wayland-user-smoke.md for the discipline that keeps this
# script current: every user-facing feature PR MUST add or update a check
# here in the same PR.
#
# Runs cleanly outside Wayland too (WSL, Git Bash, a Linux X11 box): the
# environment section reports the actual session type and continues.
#
# Deliberately uses `set -uo pipefail` — NOT `-e` — so one failing check
# does not skip the remaining sections. Each section reports its own ✓/✗/⚠.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIXTURE_WAV="${REPO_ROOT}/src/python/tests/fixtures/hello.wav"

pass=0
fail=0
skip=0

# --- colour helpers (auto-disable when stdout isn't a TTY) ---
if [ -t 1 ]; then
    C_BOLD_CYAN='\033[1;36m'
    C_GREEN='\033[32m'
    C_RED='\033[31m'
    C_YELLOW='\033[33m'
    C_RESET='\033[0m'
else
    C_BOLD_CYAN=''
    C_GREEN=''
    C_RED=''
    C_YELLOW=''
    C_RESET=''
fi

section() { printf '\n%b== %s ==%b\n' "$C_BOLD_CYAN" "$*" "$C_RESET"; }
ok()      { printf '  %b✓%b %s\n' "$C_GREEN" "$C_RESET" "$*"; pass=$((pass+1)); }
bad()     { printf '  %b✗%b %s\n' "$C_RED" "$C_RESET" "$*"; fail=$((fail+1)); }
warn()    { printf '  %b⚠%b %s (skipped)\n' "$C_YELLOW" "$C_RESET" "$*"; skip=$((skip+1)); }
info()    { printf '    %s\n' "$*"; }

# --------------------------------------------------------------------------
# Detect Wayland/X11/other session
# --------------------------------------------------------------------------
detect_session() {
    if [ "${XDG_SESSION_TYPE:-}" = "wayland" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
        echo "wayland"
    elif [ "${XDG_SESSION_TYPE:-}" = "x11" ] || [ -n "${DISPLAY:-}" ]; then
        echo "x11"
    elif grep -qi microsoft /proc/version 2>/dev/null; then
        echo "wsl"
    else
        echo "unknown"
    fi
}

# --------------------------------------------------------------------------
# Detect which whisper-dictate command to use
#   1) installed `whisper-dictate` on PATH (Rust CLI)
#   2) Python fallback: PYTHONPATH=repo/src/python python3 -m whisper_dictate.vp_cli
#
# The Python fallback exposes a subset of the shipped surface (the
# argparse-based flags in vp_cli.py) — enough to exercise --simulate-ptt
# and a few flag-only checks, but NOT the Rust subcommands like
# `models list` or `config show`. Those sections warn-skip when only
# the Python fallback is available.
# --------------------------------------------------------------------------
CMD_SOURCE=""   # "installed" | "source" | "none"
CMD_MODE=""     # "rust" | "python"

detect_command() {
    if command -v whisper-dictate >/dev/null 2>&1; then
        CMD_SOURCE="installed"
        CMD_MODE="rust"
    elif [ -d "${REPO_ROOT}/src/python/whisper_dictate" ] \
         && command -v python3 >/dev/null 2>&1; then
        CMD_SOURCE="source"
        CMD_MODE="python"
    else
        CMD_SOURCE="none"
        CMD_MODE=""
    fi
}

# Run the CLI with the detected command. First arg is the subcommand path
# (e.g. "models list") for the Rust binary — the Python fallback translates
# a handful of known subcommands into their argparse-flag equivalents and
# warn-skips the rest by returning 127.
run_cli() {
    local subcmd="$1"; shift
    case "$CMD_MODE" in
        rust)
            # shellcheck disable=SC2086
            whisper-dictate $subcmd "$@"
            ;;
        python)
            case "$subcmd" in
                "--version")
                    if [ -f "${REPO_ROOT}/VERSION" ]; then
                        printf 'whisper-dictate %s (from VERSION file)\n' \
                            "$(cat "${REPO_ROOT}/VERSION")"
                    else
                        PYTHONPATH="${REPO_ROOT}/src/python" python3 -c \
                            "import whisper_dictate; print(getattr(whisper_dictate, '__version__', 'unknown'))"
                    fi
                    ;;
                "simulate-ptt")
                    PYTHONPATH="${REPO_ROOT}/src/python" python3 -m \
                        whisper_dictate.vp_simulate_ptt "$@"
                    ;;
                *)
                    return 127
                    ;;
            esac
            ;;
        *)
            return 127
            ;;
    esac
}

# --------------------------------------------------------------------------
# SECTION: Environment
# --------------------------------------------------------------------------
section "Environment"
SESSION="$(detect_session)"
info "session type       : $SESSION"
info "XDG_SESSION_TYPE   : ${XDG_SESSION_TYPE:-(unset)}"
info "WAYLAND_DISPLAY    : ${WAYLAND_DISPLAY:-(unset)}"
info "DISPLAY            : ${DISPLAY:-(unset)}"
info "python3            : $(python3 --version 2>&1 || echo missing)"

detect_command
info "whisper-dictate    : $(command -v whisper-dictate 2>/dev/null || echo '(not on PATH)')"
info "command source     : $CMD_SOURCE ($CMD_MODE)"

if [ "$CMD_SOURCE" = "none" ]; then
    bad "cannot locate whisper-dictate (no installed binary, no src/python tree)"
    printf '\n'
    printf 'Nothing else to check — aborting.\n'
    exit 1
fi

if [ "$SESSION" != "wayland" ]; then
    info "note: not a Wayland session — running headless-compatible checks anyway"
fi

# --------------------------------------------------------------------------
# SECTION: --version
# --------------------------------------------------------------------------
section "whisper-dictate --version"
if out="$(run_cli --version 2>&1)"; then
    version_line="$(printf '%s\n' "$out" | head -n 1)"
    ok "returned: $version_line"
else
    rc=$?
    bad "exit $rc"
    info "$out"
fi

# --------------------------------------------------------------------------
# SECTION: models list
# --------------------------------------------------------------------------
section "models list (curated Whisper catalog)"
if [ "$CMD_MODE" = "python" ]; then
    warn "models list is a Rust subcommand — not exposed by the Python fallback"
else
    if out="$(run_cli "models list" 2>&1)"; then
        if printf '%s' "$out" | grep -q "tiny.en" && \
           printf '%s' "$out" | grep -q "large-v3"; then
            ok "catalog lists tiny.en and large-v3"
        else
            bad "catalog missing tiny.en and/or large-v3 entries"
            info "$out"
        fi
    else
        rc=$?
        bad "exit $rc"
        info "$out"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: devices test (mic-open probe against the system default)
#
# `devices test <NAME>` (PR #495) opens the cpal input stream against a
# named device (empty string = system default) and reports back — fast
# check that the audio subsystem is reachable before the heavier
# simulate-ptt run. A missing device on a headless box is not a hard
# fail: the check downgrades to warn-skip so the smoke stays green on
# CI runners with no audio hardware.
# --------------------------------------------------------------------------
section "devices test (default device)"
if [ "$CMD_MODE" = "python" ]; then
    warn "devices test is a Rust subcommand — not exposed by the Python fallback"
else
    dev_out="$(whisper-dictate devices test "" 2>&1)"
    dev_rc=$?
    if [ "$dev_rc" -eq 0 ]; then
        ok "devices test runs against the system default"
    else
        # Not a hard fail when there's no audio hardware (headless CI):
        # match cpal / whisper-dictate's usual "no device" phrasings and
        # downgrade to a warn-skip. Anything else is a real regression.
        if printf '%s' "$dev_out" | grep -qi "not found\|no device\|no audio\|no input\|no default"; then
            warn "no default audio device (headless environment expected)"
        else
            bad "devices test failed unexpectedly (exit $dev_rc)"
            info "$(printf '%s\n' "$dev_out" | head -n 3)"
        fi
    fi
fi

# --------------------------------------------------------------------------
# SECTION: simulate-ptt (headless dictation pipeline)
# --------------------------------------------------------------------------
section "simulate-ptt (fixture WAV, dry-run, tiny.en, CPU)"
if [ ! -f "$FIXTURE_WAV" ]; then
    warn "fixture WAV missing: $FIXTURE_WAV"
else
    if [ "$CMD_MODE" = "rust" ]; then
        # Rust subcommand: --language, --model, --wav; no --device switch,
        # so pin CPU via env so the check never depends on a GPU being
        # present. --dry-run is the default (no --inject).
        out="$(VOICEPI_DEVICE=cpu whisper-dictate simulate-ptt \
                    --wav "$FIXTURE_WAV" \
                    --model tiny.en \
                    --language en \
                    --json 2>&1)"
        rc=$?
    else
        # Python fallback: --wav, --dry-run, --model, --device, --lang, --json
        out="$(PYTHONPATH="${REPO_ROOT}/src/python" python3 -m \
                    whisper_dictate.vp_simulate_ptt \
                    --wav "$FIXTURE_WAV" \
                    --dry-run \
                    --model tiny.en \
                    --device cpu \
                    --lang en \
                    --json 2>&1)"
        rc=$?
    fi

    if [ "$rc" -eq 0 ]; then
        if printf '%s' "$out" | grep -q "simulate_ptt\|simulate-ptt"; then
            ok "pipeline exit 0, simulate_ptt event/tag present"
            info "(empty transcription on a synthetic tone is expected — checking pipeline plumbing, not ASR)"
        else
            bad "exit 0 but simulate_ptt marker not seen in output"
            info "$out"
        fi
    else
        bad "exit $rc"
        info "$out"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: config get/set — PENDING (audit item 2)
# --------------------------------------------------------------------------
section "config get/set"
if [ "$CMD_MODE" = "rust" ] && whisper-dictate config --help >/dev/null 2>&1; then
    if out="$(whisper-dictate config path 2>&1)" && [ -n "$out" ]; then
        ok "config path resolves: $out"
    else
        rc=$?
        bad "config path exit $rc"
        info "$out"
    fi
else
    warn "config CLI not available on this build — pending audit item 2"
fi

# --------------------------------------------------------------------------
# SECTION: doctor / hotkey-listener startup smoke — PENDING
# --------------------------------------------------------------------------
section "doctor (platform readiness)"
if [ "$CMD_MODE" = "rust" ] && whisper-dictate doctor --help >/dev/null 2>&1; then
    if out="$(whisper-dictate doctor 2>&1)"; then
        ok "doctor exit 0"
    else
        rc=$?
        # doctor is allowed to report warnings without failing the smoke;
        # a non-zero exit here still counts as a fail so we notice.
        bad "doctor exit $rc"
        info "$out"
    fi
else
    warn "doctor subcommand not on this build — pending audit item 2"
fi

# --------------------------------------------------------------------------
# SECTION: hotkey listener verify — PENDING
# --------------------------------------------------------------------------
section "hotkey listener startup smoke"
warn "no --verify-hotkey-only flag yet — pending audit item 2"

# --------------------------------------------------------------------------
# SECTION: injection dry-run per backend — PENDING
# --------------------------------------------------------------------------
section "injection dry-run (ydotool / wtype)"
warn "inject-text CLI dry-run not yet exposed — pending audit item 2"

# --------------------------------------------------------------------------
# Summary
# --------------------------------------------------------------------------
section "Summary"
printf '  Passed:  %d\n  Failed:  %d\n  Skipped: %d\n' "$pass" "$fail" "$skip"

if [ "$fail" -eq 0 ]; then
    exit 0
else
    exit 1
fi
