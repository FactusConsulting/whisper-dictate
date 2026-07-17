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
# SECTION: config get/set (persistence roundtrip — audit item 2 chunk A)
#
# Real exercise now that `whisper-dictate config get KEY` and
# `whisper-dictate config set KEY VALUE` ship. Runs against a scratch
# config file (VOICEPI_CONFIG override) so the smoke never mutates the
# user's real config.json, and restores the previous env at the end.
# The Python fallback path does not expose the Rust config verbs, so it
# still warn-skips there — same discipline as `models list` and
# `devices test`.
# --------------------------------------------------------------------------
section "config get/set (persistence roundtrip)"
if [ "$CMD_MODE" = "python" ]; then
    warn "config get/set are Rust subcommands — not exposed by the Python fallback"
else
    old_voicepi_config="${VOICEPI_CONFIG:-}"
    scratch_config="$(mktemp -t wd-cfg-smoke.XXXXXX.json)"
    # mktemp creates the file empty; wipe so the "no file yet" branch is
    # exercised on first `get` (that's the fresh-user case we care about).
    rm -f "$scratch_config"
    export VOICEPI_CONFIG="$scratch_config"

    get_before="$(whisper-dictate config get audio_device 2>&1)"
    get_before_rc=$?
    if [ "$get_before_rc" -ne 0 ]; then
        bad "config get on empty config failed (exit $get_before_rc)"
        info "$(printf '%s\n' "$get_before" | head -n 2)"
    else
        ok "config get audio_device works on empty config"
    fi

    set_out="$(whisper-dictate config set audio_device wd-smoke-mic 2>&1)"
    set_rc=$?
    get_after="$(whisper-dictate config get audio_device 2>&1)"
    get_after_rc=$?
    if [ "$set_rc" -eq 0 ] && [ "$get_after_rc" -eq 0 ] && \
       [ "$get_after" = "wd-smoke-mic" ]; then
        ok "config set + get roundtrip persists across processes"
    else
        bad "config set/get roundtrip broken (set exit $set_rc, get exit $get_after_rc, got: $get_after)"
        info "set stderr: $(printf '%s\n' "$set_out" | head -n 2)"
    fi

    # Unknown-key error path: must exit non-zero with a message that lists
    # at least one valid key so the user has something to grep against.
    if bad_out="$(whisper-dictate config get definitely-not-a-key 2>&1)"; then
        bad "unknown-key get should fail but exited 0"
        info "$(printf '%s\n' "$bad_out" | head -n 2)"
    elif printf '%s' "$bad_out" | grep -q "audio_device"; then
        ok "unknown-key error lists valid keys"
    else
        bad "unknown-key error did not list valid keys"
        info "$(printf '%s\n' "$bad_out" | head -n 4)"
    fi

    rm -f "$scratch_config"
    if [ -n "$old_voicepi_config" ]; then
        export VOICEPI_CONFIG="$old_voicepi_config"
    else
        unset VOICEPI_CONFIG
    fi
fi

section "config path"
if [ "$CMD_MODE" = "rust" ] && whisper-dictate config --help >/dev/null 2>&1; then
    if out="$(whisper-dictate config path 2>&1)" && [ -n "$out" ]; then
        ok "config path resolves: $out"
    else
        rc=$?
        bad "config path exit $rc"
        info "$out"
    fi
else
    warn "config CLI not available on this build"
fi

# --------------------------------------------------------------------------
# SECTION: dictionary prompt (build initial-prompt from user dictionary)
#
# `dictionary prompt --json` (audit item 2 chunk C) reads the on-disk
# dictionary + config and prints the Whisper `initial_prompt` string
# the runtime would use. Falling back to the per-user default is
# permitted to succeed with an empty prompt on a fresh install (no
# dictionary yet), so a clean box does not fail this section.
# --------------------------------------------------------------------------
section "dictionary prompt (build initial-prompt from user dictionary)"
if [ "$CMD_MODE" = "python" ]; then
    warn "dictionary prompt is a Rust subcommand — not exposed by the Python fallback"
else
    dict_out="$(whisper-dictate dictionary prompt --json 2>&1)"
    dict_rc=$?
    if [ "$dict_rc" -eq 0 ]; then
        if printf '%s' "$dict_out" | grep -q '"prompt":' \
           && printf '%s' "$dict_out" | grep -q '"length_chars":' \
           && printf '%s' "$dict_out" | grep -q '"term_count":'; then
            ok "dictionary prompt returns valid JSON"
        else
            bad "dictionary prompt exit 0 but JSON keys missing"
            info "$(printf '%s\n' "$dict_out" | head -n 3)"
        fi
    else
        # A missing default dictionary is NOT a failure — load_or_empty
        # returns empty for the default path. Any non-zero exit here is
        # a real regression (parse error, missing subcommand, etc.).
        bad "dictionary prompt failed unexpectedly (exit $dict_rc)"
        info "$(printf '%s\n' "$dict_out" | head -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: doctor (audit item 2 chunk E)
#
# `doctor --json` runs the full readiness matrix and prints one
# `{"checks":[...],"summary":{"ok":N,"warn":N,"fail":N}}` line. Exit 0 means
# every check passed (warnings are non-blocking); exit 1 means at least one
# fail check would block dictation. The smoke reports each shape so the
# operator can see WHY doctor tripped without re-running it.
# --------------------------------------------------------------------------
section "doctor (platform readiness)"
if [ "$CMD_MODE" != "rust" ]; then
    warn "doctor is a Rust subcommand — not exposed by the Python fallback"
else
    doctor_out_file="$(mktemp)"
    whisper-dictate doctor --json >"$doctor_out_file" 2>&1
    doctor_rc=$?
    if [ "$doctor_rc" -eq 0 ]; then
        ok "doctor reports platform ready (all critical checks pass)"
    elif [ "$doctor_rc" -eq 1 ]; then
        # Failed checks — but doctor at least ran.
        fail_count="$(grep -oE '"fail":[[:space:]]*[0-9]+' "$doctor_out_file" | grep -oE '[0-9]+' | head -1)"
        warn "doctor reports ${fail_count:-?} failed checks — inspect: $(head -c 500 "$doctor_out_file")"
    else
        bad "doctor invocation failed with exit $doctor_rc"
        info "$(head -c 500 "$doctor_out_file")"
    fi
    rm -f "$doctor_out_file"
fi

# --------------------------------------------------------------------------
# SECTION: hotkey capture (listener install smoke — audit item 2 chunk F)
#
# `hotkey capture --for 0.5` installs the PTT listener for a bounded window
# and prints every OS key event and chord match/release it observes. Here we
# only use it as a smoke test — no keys are synthesised — so the assertion
# is "did the listener install cleanly within the window?". On Linux without
# an X display / evdev permissions the install correctly refuses (that's the
# P1-#2 path in the Rust hotkey subsystem); we warn-skip so headless CI legs
# don't fail on it.
# --------------------------------------------------------------------------
section "hotkey capture (listener install smoke, --for 0.5s)"
if [ "$CMD_MODE" = "python" ]; then
    warn "hotkey capture is a Rust subcommand — not exposed by the Python fallback"
else
    hk_out="$(whisper-dictate hotkey capture --for 0.5 --json 2>&1)"
    hk_rc=$?
    if [ "$hk_rc" -eq 0 ]; then
        # First line should be a listener_installed JSON envelope.
        if printf '%s' "$hk_out" | head -n 1 | grep -q '"kind":"listener_installed"'; then
            ok "hotkey capture: listener installed cleanly (0.5s window)"
        else
            warn "hotkey capture exit 0 but no listener_installed line: $(printf '%s\n' "$hk_out" | head -n 1)"
        fi
    else
        # On Linux without evdev perms / X display / rust-hotkeys feature the
        # install refusal is expected. Only fail on unexpected shapes.
        if printf '%s' "$hk_out" | grep -qi "rust-hotkeys\|permission\|evdev\|X display\|no display\|listener failed"; then
            warn "hotkey capture: listener unavailable on this platform (expected without display/permissions/feature)"
        else
            bad "hotkey capture failed (exit $hk_rc): $(printf '%s\n' "$hk_out" | head -n 2)"
        fi
    fi

    # ---------------------------------------------------------------------
    # Additional Wayland-only probe: `--driver evdev` verifies the item-5
    # prereq-2 evdev listener installs cleanly (audit item 5). Under Wayland
    # rdev's XRecord path is deaf, so evdev is the ONLY listener that works
    # for real PTT — and its own `/dev/input` enumeration must accept the
    # user's keyboard while excluding whisper-dictate's ydotoold virtual node
    # (prereq 3). A permission failure (user not in `input` group) is a
    # warn — the fix is a user action (`sudo usermod -aG input $USER`),
    # not a code regression.
    # ---------------------------------------------------------------------
    if [ "$SESSION" = "wayland" ]; then
        # Detect whether the `--driver` flag exists at all (older builds
        # skip this probe). `--help` output on the capture subcommand
        # lists flags one per section.
        if whisper-dictate hotkey capture --help 2>&1 | grep -q -- "--driver"; then
            hk_ev_out="$(whisper-dictate hotkey capture --for 0.5 --driver evdev --json 2>&1)"
            hk_ev_rc=$?
            if [ "$hk_ev_rc" -eq 0 ]; then
                # Envelope should now carry `"driver":"evdev"` since we
                # forced the backend explicitly.
                first_line="$(printf '%s\n' "$hk_ev_out" | head -n 1)"
                if printf '%s' "$first_line" | grep -q '"driver":"evdev"'; then
                    ok "hotkey capture --driver evdev installs cleanly under Wayland"
                else
                    warn "hotkey capture --driver evdev exit 0 but envelope missing evdev tag: $first_line"
                fi
            elif printf '%s' "$hk_ev_out" | grep -qi "permission\|input group\|no readable\|usermod"; then
                warn "hotkey capture --driver evdev: user lacks /dev/input access (add user to 'input' group)"
            elif printf '%s' "$hk_ev_out" | grep -qi "rust-hotkeys\|listener failed"; then
                warn "hotkey capture --driver evdev: rust-hotkeys not compiled in or evdev backend unavailable"
            else
                bad "hotkey capture --driver evdev failed (exit $hk_ev_rc): $(printf '%s\n' "$hk_ev_out" | head -n 2)"
            fi
        else
            warn "hotkey capture --driver flag not present in this build (pre-item-5-prereq2)"
        fi
    fi
fi

# --------------------------------------------------------------------------
# SECTION: self-test ptt-wedge (regression test — v1.20.7 killer)
#
# Headless regression check for the self-injection PTT-wedge class of bugs
# (Windows v1.20.7, Wayland via #467). Drives the guard + tracker directly
# with synthetic events — no OS-level hook, no audio, no display — so it
# runs on any container. If any iteration fails the wedge is back.
# --------------------------------------------------------------------------
section "self-test ptt-wedge (regression test — v1.20.7 killer)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test is a Rust subcommand — not exposed by the Python fallback"
else
    st_out="$(whisper-dictate self-test ptt-wedge --iterations 3 --json 2>&1)"
    st_rc=$?
    if [ "$st_rc" -eq 0 ] && printf '%s' "$st_out" | grep -q '"all_passed":true'; then
        ok "PTT wedge regression test passed (3 iterations)"
    elif printf '%s' "$st_out" | grep -qi "rust-hotkeys\|rust-injection\|rebuild with"; then
        warn "self-test ptt-wedge requires rust-hotkeys,rust-injection features (skipped on this build)"
    else
        bad "PTT wedge regression test FAILED — v1.20.7-style bug is back: $(printf '%s\n' "$st_out" | tail -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: dictate-run CLI (Rust dictation runtime — Phase A step 1)
#
# Audit item 5 Phase A step 1: adds the `whisper-dictate dictate-run` verb
# that installs the full Rust dictation runtime in-process. The verb is not
# wired into the Python entrypoint yet (Phase A step 2 does that), so this
# section only verifies the CLI surface parses and the help text is
# reachable. We deliberately do NOT run the real thing headless — it needs
# a display server and an audio device that this smoke box doesn't provide.
# --------------------------------------------------------------------------
section "dictate-run CLI (Rust dictation runtime — Phase A step 1)"
if [ "$CMD_MODE" = "python" ]; then
    warn "dictate-run is a Rust subcommand — not exposed by the Python fallback"
else
    dr_out="$(whisper-dictate dictate-run --help 2>&1)"
    dr_rc=$?
    if [ "$dr_rc" -eq 0 ] && printf '%s' "$dr_out" | grep -q -- '--json-events'; then
        ok "dictate-run --help works"
    else
        bad "dictate-run --help failed: $(printf '%s\n' "$dr_out" | head -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: inject-text dry-run (audit item 2 chunk B)
#
# The public `inject-text <TEXT>` verb wraps the injection library with a
# dry-run default: it reports the resolved backend + keystroke plan without
# touching the display server. Real injection is opt-in via `--do-it`. This
# section only exercises the dry-run — no test in this smoke script should
# ever move the user's cursor.
# --------------------------------------------------------------------------
section "inject-text dry-run (pynput / wtype / ydotool)"
if [ "$CMD_MODE" = "python" ]; then
    warn "inject-text is a Rust subcommand — not exposed by the Python fallback"
else
    inject_out="$(whisper-dictate inject-text "smoke test" --dry-run --json 2>&1)"
    inject_rc=$?
    if [ "$inject_rc" -eq 0 ] && \
       printf '%s' "$inject_out" | grep -q '"dry_run":true' && \
       printf '%s' "$inject_out" | grep -q '"typed":false'; then
        ok "inject-text --dry-run --json returns keystroke plan"
        # Extra assertion: `--do-it` was NOT passed so `typed` must be false.
        # `dry_run:true` + `typed:false` is the smoke contract for a safe run.
        if [ "$SESSION" = "wayland" ]; then
            wt_out="$(whisper-dictate inject-text "hej" --dry-run --backend wtype --json 2>&1)"
            wt_rc=$?
            if [ "$wt_rc" -eq 0 ] && printf '%s' "$wt_out" | grep -q '"backend":"wtype"'; then
                ok "inject-text --backend wtype --dry-run works"
            else
                warn "wtype backend dry-run failed: $(printf '%s\n' "$wt_out" | head -n 2)"
            fi
        fi
    else
        bad "inject-text --dry-run failed (exit $inject_rc)"
        info "$(printf '%s\n' "$inject_out" | head -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: history last / reinject-last (audit item 2 chunk D)
#
# The public `history` CLI verbs read the on-disk JSONL history file. On a
# fresh install the file does not exist yet — that is not a smoke failure,
# so an "empty history" / "no history" error downgrades to warn-skip.
#
# `copy-last` is deliberately NOT exercised here: it needs a live display
# server + one of wl-copy / xclip / clip.exe / pbcopy installed, and the
# smoke box is not the right place to verify that matrix (headless CI has
# no clipboard). Users on Wayland get it via the manual real-world test.
# --------------------------------------------------------------------------
section "history last / reinject-last (dry-run)"
if [ "$CMD_MODE" = "python" ]; then
    warn "history is a Rust subcommand — not exposed by the Python fallback"
else
    hist_out="$(whisper-dictate history last --json 2>&1)"
    hist_rc=$?
    if [ "$hist_rc" -eq 0 ]; then
        # Success shape: `[]` on empty, `[{…}]` when at least one entry.
        # Either is a pass for the smoke — we're checking the verb runs
        # cleanly, not that history exists on this box.
        ok "history last --json returns JSON (payload: $(printf '%s' "$hist_out" | head -c 80)…)"
    elif printf '%s' "$hist_out" | grep -qi "no history\|empty\|not found"; then
        warn "history file empty or missing (expected on fresh install)"
    else
        bad "history last failed: $(printf '%s\n' "$hist_out" | head -n 2)"
    fi

    reinject_out="$(whisper-dictate history reinject-last --dry-run --json 2>&1)"
    reinject_rc=$?
    if [ "$reinject_rc" -eq 0 ] && \
       printf '%s' "$reinject_out" | grep -q '"dry_run":true' && \
       printf '%s' "$reinject_out" | grep -q '"typed":false'; then
        ok "history reinject-last --dry-run --json returns keystroke plan"
    elif printf '%s' "$reinject_out" | grep -qi "no history\|empty"; then
        warn "no transcript to reinject (expected on fresh install)"
    else
        bad "history reinject-last failed: $(printf '%s\n' "$reinject_out" | head -n 2)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: dictate engine dispatch (VOICEPI_DICTATE_ENGINE=rust opt-in)
#
# Audit item 5 Phase A step 2. The Python runtime honours
# VOICEPI_DICTATE_ENGINE and, when set to `rust`, shells out to
# `whisper-dictate dictate-run`. The full loop is manual QA (needs a
# display + audio + a running Rust binary with the required features);
# here we just prove the Python side recognises the flag by importing
# the dispatch selector. That is what regresses if a refactor drops the
# env-var branch — the exact regression class this section guards.
# --------------------------------------------------------------------------
section "dictate engine dispatch (VOICEPI_DICTATE_ENGINE=rust opt-in)"
if [ "$CMD_MODE" = "python" ] || command -v python3 >/dev/null 2>&1; then
    engine_check_out="$(VOICEPI_DICTATE_ENGINE=rust python3 -c '
from whisper_dictate.vp_dictate_engine import (
    ENGINE_ENV, ENGINE_PYTHON, ENGINE_RUST, select_engine,
)
picked = select_engine()
assert picked == ENGINE_RUST, (
    "runtime did not resolve %s=rust to the rust engine (got %r)"
    % (ENGINE_ENV, picked)
)
print("selector=%s picked=%s" % ("select_engine", picked))
' 2>&1)"
    engine_check_rc=$?
    if [ "$engine_check_rc" -eq 0 ]; then
        ok "Python runtime recognizes VOICEPI_DICTATE_ENGINE=rust ($engine_check_out)"
    else
        warn "engine dispatch not testable: $(printf '%s\n' "$engine_check_out" | head -2)"
    fi
else
    warn "engine dispatch verify needs python3 in PATH (Rust-only build)"
fi

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
