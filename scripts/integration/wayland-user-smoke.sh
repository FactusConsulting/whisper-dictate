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

# Cargo features observed at run time from the self-test sections below, so a
# later section can tell a genuinely-capable binary from one that merely
# starts. Tri-state on purpose: "unknown" means the probing section did not
# get far enough to say (e.g. whisper-load skipped for a missing model
# fixture, which reveals nothing about the feature), and must not be read as
# either present or absent.
#
# Only `whisper-rs-local` is tracked this way, because `self-test whisper-load`
# is gated on exactly that feature. There is deliberately NO audio flag here:
# `self-test audio-capture` is gated on `audio-capture` (main.rs:322,330), and
# Cargo.toml:79 makes `audio-in-rust` imply `audio-capture` and not the
# converse — so that verb succeeding proves nothing about `audio-in-rust`.
# A missing `audio-in-rust` is detected where it is actually observable: the
# in-process runtime section reads it off the stub-fallback event, which names
# the missing feature in its own message.
FEATURE_WHISPER_RS_LOCAL=unknown

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
# Which hotkey driver the Rust binary will select — a shell mirror of
# `hotkey::manager::resolve_driver` + `is_wayland_session`
# (src/rust/hotkey/manager/mod.rs:220-250). Used to decide whether a listener
# refusal is the EXPECTED no-display rdev failure or a real defect.
#
# Kept faithful to the Rust in three details that a looser reading gets wrong:
#   1. An explicit `VOICEPI_HOTKEY_DRIVER` wins outright — session detection
#      never overrides it (that is the documented escape hatch).
#   2. `DriverKind::parse` trims, lower-cases, and accepts ALIASES: `x11` is a
#      synonym for rdev and `wayland` for evdev. Recognising only the
#      canonical spellings makes the mirror disagree with the binary in both
#      directions — `=wayland` on a headless box selects evdev in Rust, and
#      reading it as rdev here would warn-skip a genuine evdev failure.
#      An unrecognised value (or `auto`/empty) falls back to Auto, i.e. to
#      session detection, which is what the fall-through below does.
#   3. `is_wayland_session()` is an OR of XDG_SESSION_TYPE=wayland (matched
#      case-insensitively, NOT trimmed) and a WAYLAND_DISPLAY that is
#      non-empty AFTER TRIMMING. Either alone selects evdev, so a Wayland box
#      that exports only the session type still gets evdev — while a
#      whitespace-only WAYLAND_DISPLAY counts as unset and leaves rdev
#      selected. The asymmetry is real: the Rust trims one and not the other.
#
# See `resolve_hotkey_driver_selftest` below for the regression cases.
# --------------------------------------------------------------------------
resolve_hotkey_driver() {
    # Mirror of `DriverKind::parse`: trim surrounding whitespace, fold case.
    _drv="$(printf '%s' "${VOICEPI_HOTKEY_DRIVER:-}" \
            | tr '[:upper:]' '[:lower:]' \
            | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
    case "$_drv" in
        rdev|x11)     echo "rdev";  return ;;
        evdev|wayland) echo "evdev"; return ;;
    esac
    # `auto`, empty, or unrecognised — session detection, Linux only. On any
    # other OS the Rust falls through to rdev unconditionally.
    if [ "$(uname -s 2>/dev/null)" != "Linux" ]; then
        echo "rdev"; return
    fi
    # XDG_SESSION_TYPE: case-folded, NOT trimmed (mirrors eq_ignore_ascii_case).
    _xdg="$(printf '%s' "${XDG_SESSION_TYPE:-}" | tr '[:upper:]' '[:lower:]')"
    # WAYLAND_DISPLAY: trimmed before the emptiness test (mirrors
    # `!v.trim().is_empty()`), so a whitespace-only value counts as unset.
    _wl="$(printf '%s' "${WAYLAND_DISPLAY:-}" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
    if [ "$_xdg" = "wayland" ] || [ -n "$_wl" ]; then
        echo "evdev"
    else
        echo "rdev"
    fi
}

# Regression cases for the mirror above. Pure function, no I/O — runs on every
# invocation so a drift between this helper and `DriverKind::parse` /
# `resolve_driver` is caught on the operator's own box rather than silently
# mis-classifying a listener failure later in the run. Reports through the
# normal ok/bad counters.
resolve_hotkey_driver_selftest() {
    _drv_fails=""
    _drv_case() {  # <expected> <driver-env> <xdg> <wayland-display>
        _got="$(VOICEPI_HOTKEY_DRIVER="$2" XDG_SESSION_TYPE="$3" \
                WAYLAND_DISPLAY="$4" resolve_hotkey_driver)"
        [ "$_got" = "$1" ] || _drv_fails="${_drv_fails}
      driver=$(printf '%q' "$2") xdg=$(printf '%q' "$3") wl=$(printf '%q' "$4") -> $_got, expected $1"
    }

    # Session detection is Linux-only in the Rust (`#[cfg(target_os = "linux")]`
    # around the Auto arm); every other platform falls through to rdev
    # unconditionally. So the expectation for a Wayland-looking environment is
    # NOT a constant — on Git Bash, which this script explicitly supports,
    # `$WAYLAND` below is rdev and hard-coding evdev would fail the self-test
    # and take the whole run down on a perfectly healthy box.
    if [ "$(uname -s 2>/dev/null)" = "Linux" ]; then
        WAYLAND_AUTO=evdev
    else
        WAYLAND_AUTO=rdev
    fi

    # -- Auto / session detection (platform-dependent expectations) ----------
    #         expect         DRIVER      XDG        WAYLAND_DISPLAY
    _drv_case rdev           ""          ""         ""
    _drv_case "$WAYLAND_AUTO" ""         "wayland"  ""
    _drv_case "$WAYLAND_AUTO" ""         "Wayland"  ""
    _drv_case "$WAYLAND_AUTO" ""         ""         "wayland-0"
    _drv_case rdev           ""          "x11"      ""
    _drv_case rdev           "auto"      ""         ""
    _drv_case "$WAYLAND_AUTO" "auto"     "wayland"  ""
    # XDG_SESSION_TYPE is compared WITHOUT trimming in the Rust, so a padded
    # value does not count as Wayland.
    _drv_case rdev           ""          " wayland " ""
    # WAYLAND_DISPLAY *is* trimmed, so whitespace-only counts as unset.
    _drv_case rdev           ""          ""         "   "
    _drv_case "$WAYLAND_AUTO" ""         ""         "  wayland-0  "
    # Unrecognised driver values fall back to Auto, i.e. session detection.
    _drv_case "$WAYLAND_AUTO" "nonsense" "wayland"  ""
    _drv_case rdev           "nonsense"  ""         ""

    # -- Explicit overrides (platform-independent: parsed before detection) --
    _drv_case evdev   "evdev"     ""         ""
    _drv_case rdev    "rdev"      "wayland"  "wayland-0"
    # Aliases (DriverKind::parse) — the case this self-test exists for.
    _drv_case evdev   "wayland"   ""         ""
    _drv_case rdev    "x11"       "wayland"  "wayland-0"
    # Whitespace + case folding.
    _drv_case evdev   "  evdev  " ""         ""
    _drv_case rdev    "  X11 "    "wayland"  ""
    _drv_case evdev   "EVDEV"     ""         ""

    if [ -z "$_drv_fails" ]; then
        ok "hotkey-driver resolution mirrors DriverKind::parse + resolve_driver (auto=$WAYLAND_AUTO on $(uname -s 2>/dev/null || echo unknown))"
    else
        bad "hotkey-driver mirror has drifted from the Rust:$_drv_fails"
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

info "hotkey driver      : $(resolve_hotkey_driver) (as the Rust binary would resolve it)"
resolve_hotkey_driver_selftest

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
        # `models list` shows the USER-FACING catalog only; hidden entries
        # (legacy sizes + the tiny fixture CI downloads) are deliberately
        # absent. Parse the exact NAME column of each rendered row rather than
        # substring-matching the whole output: "large-v3" is a substring of
        # "large-v3-turbo", so a naive grep passes even when large-v3 is gone.
        # Every check below runs independently — an earlier success must not
        # short-circuit the hidden-leak check.
        listed="$(printf '%s\n' "$out" | sed -n 's/^[[:space:]]*\[[^]]*\][[:space:]]*\([^[:space:]]*\).*/\1/p')"
        catalog_ok=1
        for want in large-v3-turbo large-v3; do
            if ! printf '%s\n' "$listed" | grep -qx "$want"; then
                bad "catalog missing user-facing model: $want"
                catalog_ok=0
            fi
        done
        for leak in tiny tiny.en base base.en small small.en medium; do
            if printf '%s\n' "$listed" | grep -qx "$leak"; then
                bad "catalog leaks hidden entry into the user-facing list: $leak"
                catalog_ok=0
            fi
        done
        if [ "$catalog_ok" -eq 1 ]; then
            ok "catalog lists exactly the user-facing models (large-v3-turbo, large-v3)"
        else
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
# GGML tiny-fixture resolution — for `self-test whisper-load` ONLY.
#
# Which GGML fixture is on the box depends on how it was prepared:
# .github/workflows/test.yml still downloads `tiny.en`, while newer setups
# fetch the multilingual `tiny`. Hardcoding one silently takes a "not in cache"
# skip branch on the other kind of box, letting the smoke stay green without
# exercising the loader at all. Both names stay resolvable via the hidden
# catalog entries.
#
# Deliberately NOT used by simulate-ptt: even in Rust command mode that verb
# fronts the PYTHON worker, which builds `faster_whisper.WhisperModel` against
# a separate HuggingFace/CTranslate2 cache and never reads a GGML file. Probing
# `models path` says nothing about what faster-whisper has cached, so that
# section keeps a fixed fixture instead.
# --------------------------------------------------------------------------
TINY_FIXTURE="tiny"
if [ "$CMD_MODE" = "rust" ]; then
    # Probe the CACHE DIRECTORY, never `models download` — this script must not
    # pull 78 MB behind the operator's back just to decide which name to use.
    if tiny_cache="$(whisper-dictate models path 2>/dev/null)" && [ -n "$tiny_cache" ]; then
        if [ -f "$tiny_cache/ggml-tiny.bin" ]; then
            TINY_FIXTURE="tiny"
        elif [ -f "$tiny_cache/ggml-tiny.en.bin" ]; then
            TINY_FIXTURE="tiny.en"
        fi
    fi
fi
info "tiny fixture in use: $TINY_FIXTURE"

# --------------------------------------------------------------------------
# SECTION: simulate-ptt (headless dictation pipeline)
#
# Runs against a SCRATCH config (VOICEPI_CONFIG override) plus an explicit
# `VOICEPI_STT_BACKEND=whisper`. Without both, the section is not hermetic:
# `--model tiny.en` only names the local model, while the BACKEND still comes
# from the operator's real config.json. On the maintainer's own Wayland box
# (`stt_backend=openai`, Groq base URL) the run therefore took the cloud path
# and died with "openai API requires OPENAI_API_KEY, GROQ_API_KEY, or
# VOICEPI_STT_API_KEY" — the key lives in the OS credential store and is only
# exported into the worker by the UI, never by a bare CLI verb. That is a
# false failure: the section exists to prove the LOCAL capture → transcribe →
# inject plumbing still works, which is exactly what a cloud-configured box
# never got to exercise.
#
# BOTH command modes need the isolation. The Python fallback is not flag-only:
# `vp_simulate_ptt._load_model_for_cli()` imports `vp_cli`, whose module init
# runs `apply_config_to_environ()`, after which `vp_transcribe.STT_BACKEND`
# resolves `get_value("VOICEPI_STT_BACKEND", "whisper")` from the same real
# config.json. `--model` / `--device` do not override it.
# --------------------------------------------------------------------------
section "simulate-ptt (fixture WAV, dry-run, tiny.en, CPU)"
if [ ! -f "$FIXTURE_WAV" ]; then
    warn "fixture WAV missing: $FIXTURE_WAV"
else
    # Resolve the operator's effective local-only setting BEFORE the scratch
    # config hides it, and carry it into the run below. `local_only` is a
    # PRIVACY lock: it forces the model libraries offline, so silently
    # dropping it could let a missing tiny.en be fetched from the network by
    # an operator who explicitly opted out of that.
    #
    # ENV FIRST, and passed through VERBATIM. Both engines give the ambient
    # env var precedence over the config file (`model_manager::is_local_only`
    # returns early on a truthy VOICEPI_LOCAL_ONLY; the Python side resolves
    # it through `get_value`), but `whisper-dictate config get local_only`
    # does NOT — it reports the persisted setting and its defaults, returning
    # `0` even with VOICEPI_LOCAL_ONLY=1 exported (verified). So consulting
    # the config unconditionally would overwrite an inherited `1` with `0`
    # and turn the lock OFF for this run — strictly worse than the untouched
    # inheritance this section had before.
    #
    # Verbatim rather than normalised on purpose: the two engines' truthiness
    # tables are not identical (Rust accepts 1/true/True/TRUE; Python treats
    # anything outside ""/0/false/no/off as on), so re-interpreting the value
    # here could only introduce a third reading. Passing the operator's own
    # string through lets each engine apply its own rule, exactly as it would
    # without this section's involvement.
    if [ -n "${VOICEPI_LOCAL_ONLY+set}" ]; then
        simptt_local_only="$VOICEPI_LOCAL_ONLY"
    elif [ "$CMD_MODE" = "rust" ]; then
        simptt_local_only="$(whisper-dictate config get local_only 2>/dev/null)"
    else
        simptt_local_only="$(PYTHONPATH="${REPO_ROOT}/src/python" python3 -c \
            "from whisper_dictate.vp_config import get_value
print((get_value('VOICEPI_LOCAL_ONLY') or '').strip())" 2>/dev/null)"
    fi
    # An unresolvable lookup yields the empty string, which every consumer
    # treats as "off" — the same as the pre-existing behaviour.
    : "${simptt_local_only:=}"

    # Reserve the scratch path with mktemp rather than composing one from
    # $$: a pre-existing/PID-reused `wd-simptt-smoke-<pid>.json` would be
    # read as real configuration (config values outrank env, so a stale
    # `stt_backend=openai` in it would defeat the pin below) and the
    # cleanup would delete a file this script never created. Deleting the
    # reserved file is deliberate — the loader's missing-file branch is
    # what yields built-in defaults, the same fresh-user path the config
    # section relies on.
    simptt_config="$(mktemp -t wd-simptt-smoke.XXXXXX.json)"
    rm -f "$simptt_config"

    if [ "$CMD_MODE" = "rust" ]; then
        # Rust subcommand: --language, --model, --wav; no --device switch,
        # so pin CPU via env so the check never depends on a GPU being
        # present. --dry-run is the default (no --inject).
        out="$(VOICEPI_CONFIG="$simptt_config" \
               VOICEPI_STT_BACKEND=whisper \
               VOICEPI_LOCAL_ONLY="$simptt_local_only" \
               VOICEPI_DEVICE=cpu whisper-dictate simulate-ptt \
                    --wav "$FIXTURE_WAV" \
                    --model tiny.en \
                    --language en \
                    --json 2>&1)"
        rc=$?
    else
        # Python fallback: --wav, --dry-run, --model, --device, --lang, --json
        out="$(VOICEPI_CONFIG="$simptt_config" \
               VOICEPI_STT_BACKEND=whisper \
               VOICEPI_LOCAL_ONLY="$simptt_local_only" \
               PYTHONPATH="${REPO_ROOT}/src/python" python3 -m \
                    whisper_dictate.vp_simulate_ptt \
                    --wav "$FIXTURE_WAV" \
                    --dry-run \
                    --model tiny.en \
                    --device cpu \
                    --lang en \
                    --json 2>&1)"
        rc=$?
    fi
    # Belt-and-braces: the run should never create the file, but a future
    # write-through would otherwise litter $TMPDIR.
    rm -f "$simptt_config"

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
# SECTION: self-test injection-idempotency (regression — no state leak
# between successive inject calls)
#
# Different bug class from ptt-wedge: this verb probes INJECTION-side state
# (plan-builder determinism, guard bracket counter, backend-selection cache)
# rather than the hotkey tracker. Fails if `build_plan` drifts across calls,
# if the guard's `arm_start` / `arm_end` counter leaks, or if the horizon
# stays extended past the post-grace window. Default is dry-run — no OS
# side effects — so it's safe in this smoke script and in CI. `--live`
# would type into the active window and is NEVER used here.
# --------------------------------------------------------------------------
section "self-test injection-idempotency (regression — no state leak between injects)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test is a Rust subcommand — not exposed by the Python fallback"
else
    inj_out="$(whisper-dictate self-test injection-idempotency --iterations 10 --json 2>&1)"
    inj_rc=$?
    if [ "$inj_rc" -eq 0 ] && printf '%s' "$inj_out" | grep -q '"all_passed":true'; then
        ok "injection idempotency: 10 iterations no state accumulation"
    elif printf '%s' "$inj_out" | grep -qi "rust-hotkeys\|rust-injection\|rebuild with"; then
        warn "self-test injection-idempotency requires rust-hotkeys,rust-injection features (skipped on this build)"
    else
        bad "injection idempotency FAILED — state leaks between injects: $(printf '%s\n' "$inj_out" | tail -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: self-test audio-capture (item 5 prereq 4 — cpal + PipeWire quantum)
#
# Opens the cpal input stream for 1 s, tallies samples, and reports RMS +
# peak. Applies the v1.20.6 PipeWire quantum lesson (`PIPEWIRE_QUANTUM=2048`
# when unset on Linux) BEFORE opening the stream so the fix is exercised on
# every run. Two failure modes it catches:
#   1. Stream opens but never delivers samples (v1.20.6 DMIC crash class) —
#      the verb exits non-zero.
#   2. Missing audio device / feature gate — reported as a distinctive
#      error message the section below greps for and warns on.
#
# Feature-gated behind `audio-in-rust`. On stock builds the verb refuses
# with an actionable rebuild message; we warn-skip. On feature builds
# without an audio device (headless CI containers, Wayland with mic
# muted), we also warn-skip rather than fail — the smoke script is for
# an interactive user who might not have a mic hooked up.
# --------------------------------------------------------------------------
section "self-test audio-capture (item 5 prereq 4 — cpal + PipeWire quantum)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test audio-capture is a Rust subcommand — not exposed by the Python fallback"
else
    ac_out="$(whisper-dictate self-test audio-capture --duration-ms 1000 --json 2>&1)"
    ac_rc=$?
    if [ "$ac_rc" -eq 0 ] && printf '%s' "$ac_out" | grep -q '"succeeded":true'; then
        # Extract RMS + peak with a permissive regex so a JSON-key reorder
        # doesn't blow up parsing. `sed`/`grep -oE` avoids a jq dep.
        rms="$(printf '%s' "$ac_out" | grep -oE '"rms":[^,}]+' | head -n1 | cut -d: -f2)"
        peak="$(printf '%s' "$ac_out" | grep -oE '"peak":[^,}]+' | head -n1 | cut -d: -f2)"
        quantum_branch="$(printf '%s' "$ac_out" | grep -oE '"pipewire_quantum_branch":"[^"]+"' | cut -d: -f2 | tr -d '"')"
        ok "audio-capture: 1 s captured (rms=$rms peak=$peak quantum=$quantum_branch)"
    elif printf '%s' "$ac_out" | grep -qi "requires the .audio-capture. cargo feature\|requires the .audio-in-rust. cargo feature\|rebuild with"; then
        # Note: this verb is gated on `audio-capture`, so its refusal says
        # nothing about `audio-in-rust` either. No feature flag is recorded
        # here — see the comment at the top of the script.
        warn "self-test audio-capture requires the audio-capture feature (skipped on this build)"
    elif printf '%s' "$ac_out" | grep -qi "no default input device\|input device not found\|no audio device delivered"; then
        warn "no audio device available (expected on headless / muted setups)"
    else
        bad "audio-capture FAILED unexpectedly — v1.20.6 PipeWire class may be back: $(printf '%s\n' "$ac_out" | tail -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: self-test whisper-load (regression — Whisper cold-load latency + OOM)
#
# Item 5 prereq 5: load the tiny GGML model through the same background
# preloader the supervisor will use in Phase C step 2. Regression coverage for
# the v1.20.7 silent-PTT scenario (whisper.cpp load hanging the main thread)
# + the OOM path (whisper.cpp panics on a memory-starved host, caught by
# `load_catch_unwind`). Requires the binary to be built with the
# `whisper-rs-local` feature — a stock build surfaces an actionable
# "rebuild with --features" message which this section treats as a warn/skip.
# Also skips gracefully when the model isn't cached — smoke script must NOT
# download 78MB behind the operator's back on every run.
# --------------------------------------------------------------------------
section "self-test whisper-load (Whisper cold-load latency + OOM)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test whisper-load is a Rust subcommand — not exposed by the Python fallback"
else
    # Try the multilingual fixture first, then fall back to the English-only
    # one. Which of the two is present depends on the preparation step: the
    # workflow at .github/workflows/test.yml still downloads `tiny.en`, while
    # newer setups fetch `tiny`. Hardcoding either would silently take the
    # "not in cache" warn/skip branch on the other kind of box, letting this
    # smoke stay green WITHOUT ever exercising the cold-load/OOM path this
    # section exists to cover. Both names stay resolvable via the hidden
    # catalog entries, so probing is safe.
    wl_model="$TINY_FIXTURE"
    wl_out="$(whisper-dictate self-test whisper-load --model "$wl_model" --json 2>&1)"
    wl_rc=$?
    if [ "$wl_rc" -eq 0 ] && printf '%s' "$wl_out" | grep -q '"ok":true'; then
        elapsed=$(printf '%s' "$wl_out" | grep -oE '"elapsed_ms":[0-9]+' | head -1 | cut -d: -f2)
        FEATURE_WHISPER_RS_LOCAL=yes
        ok "whisper-load: $wl_model loaded in ${elapsed:-?}ms (status=ready)"
    elif printf '%s' "$wl_out" | grep -qi "whisper-rs-local\|rebuild with"; then
        FEATURE_WHISPER_RS_LOCAL=no
        warn "self-test whisper-load requires whisper-rs-local feature (skipped on this build)"
    elif printf '%s' "$wl_out" | grep -qi "not in the cache\|models download"; then
        warn "self-test whisper-load: no tiny fixture cached — run 'whisper-dictate models download tiny' first"
    else
        bad "whisper-load FAILED — Phase B whisper backend is broken: $(printf '%s\n' "$wl_out" | tail -n 3)"
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
    engine_check_out="$(VOICEPI_DICTATE_ENGINE=rust PYTHONPATH="${REPO_ROOT}/src/python" python3 -c '
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
# SECTION: CLI reads the saved cloud API key (credential store, not just env)
#
# The keys are written by the Settings UI into the OS credential store (with
# an `api-keys.json` fallback). For a long time only the UI could read them
# back, so a cloud-configured user running the documented terminal test
# (`VOICEPI_DICTATE_ENGINE=rust whisper-dictate run`) got
#
#   x startup error: openai API requires OPENAI_API_KEY, GROQ_API_KEY, or
#     VOICEPI_STT_API_KEY/VOICEPI_POST_API_KEY
#
# on a machine where the key was saved and working in the UI.
#
# Runs entirely against SCRATCH state -- a temp store holding an obviously
# fake key, a temp config selecting the cloud backend, and the OS keyring
# disabled so the check neither reads nor writes the operator's real
# credentials. The fake key is never used for a request: the worker resolves
# it at STARTUP, which is the step that used to fail, and the run is killed
# before any dictation could happen.
# --------------------------------------------------------------------------
section "CLI reads the saved cloud API key (not just the environment)"
if [ "$CMD_MODE" != "rust" ]; then
    warn "credential-store lookup is a Rust-side behaviour - not exposed by the Python fallback"
else
    key_store="$(mktemp -t wd-keys-smoke.XXXXXX.json)"
    key_config="$(mktemp -t wd-cfg-keys-smoke.XXXXXX.json)"
    printf '{"stt-api-key:groq":"smoke-not-a-real-key"}\n' >"$key_store"
    printf '{"stt_backend":"openai","stt_base_url":"https://api.groq.com/openai/v1","stt_model":"whisper-large-v3-turbo","post_processor":"off"}\n' >"$key_config"

    # Drop every ambient key so the store is the ONLY possible source -- with
    # one exported the check would pass without exercising the lookup at all.
    key_out="$(env -u VOICEPI_STT_API_KEY -u VOICEPI_POST_API_KEY \
                   -u GROQ_API_KEY -u OPENAI_API_KEY \
                   VOICEPI_API_KEY_STORE="$key_store" \
                   VOICEPI_DISABLE_OS_KEYRING=1 \
                   VOICEPI_CONFIG="$key_config" \
               timeout --preserve-status --kill-after=2s 12s \
               whisper-dictate run 2>&1)"
    key_rc=$?
    rm -f "$key_store" "$key_config"

    if printf '%s' "$key_out" | grep -qi "requires OPENAI_API_KEY"; then
        bad "worker started without the saved key - the credential store was not consulted"
    elif printf '%s' "$key_out" | grep -qi "api ready\|state.:.opening\|listener_installed\|ready-signal"; then
        ok "cloud key resolved from the credential store (worker reached startup)"
    elif printf '%s' "$key_out" | grep -qi "no default input device\|no audio\|input device not found"; then
        warn "worker got past the key check but has no audio device (headless / muted)"
    else
        bad "worker startup shape not recognised (exit $key_rc): $(printf '%s' "$key_out" | head -c 300)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: credential lookup honors env-overridden endpoint (P1 for #615)
#
# The bug the P1 review flagged: `attach_cloud_api_keys` classified the
# credential against `settings.stt_base_url` (config value), NOT the
# endpoint the worker would actually hit after
# `worker_env_overrides()` baked in a `VOICEPI_STT_BASE_URL` override.
# Result: config says Groq, env says OpenAI, worker got the Groq key (or a
# miss) while talking to OpenAI.
#
# This check pins the fix by saving the key under the OPENAI account,
# leaving the config on Groq, and OVERRIDING the endpoint back to OpenAI
# via env. The worker must find the openai-stored key -- if it reads the
# config's Groq account instead, there is nothing to find and it dies at
# startup with the missing-key message.
#
# Deliberately cross-OS: named "wayland-user-smoke" for historical
# reasons but the credential-resolution check itself runs anywhere the
# `whisper-dictate` binary and the file-fallback store do (Linux, macOS,
# Windows Git-Bash / WSL). The store uses `VOICEPI_DISABLE_OS_KEYRING=1`
# so we never touch the operator's real OS credential manager.
# --------------------------------------------------------------------------
section "CLI classifies the credential against the effective endpoint"
if [ "$CMD_MODE" != "rust" ]; then
    warn "endpoint-override check is a Rust-side behaviour - not exposed by the Python fallback"
else
    ep_store="$(mktemp -t wd-keys-ep-smoke.XXXXXX.json)"
    ep_config="$(mktemp -t wd-cfg-ep-smoke.XXXXXX.json)"
    # Key saved for OpenAI only. Groq account is absent on purpose: if the
    # lookup falls back to the config value it will find nothing and the
    # worker will die at startup.
    printf '{"stt-api-key:openai":"smoke-openai-not-a-real-key"}\n' >"$ep_store"
    printf '{"stt_backend":"openai","stt_base_url":"https://api.groq.com/openai/v1","stt_model":"whisper-large-v3-turbo","post_processor":"off"}\n' >"$ep_config"

    ep_out="$(env -u VOICEPI_STT_API_KEY -u VOICEPI_POST_API_KEY \
                  -u GROQ_API_KEY -u OPENAI_API_KEY \
                  VOICEPI_API_KEY_STORE="$ep_store" \
                  VOICEPI_DISABLE_OS_KEYRING=1 \
                  VOICEPI_CONFIG="$ep_config" \
                  VOICEPI_STT_BASE_URL=https://api.openai.com/v1 \
              timeout --preserve-status --kill-after=2s 12s \
              whisper-dictate run 2>&1)"
    ep_rc=$?
    rm -f "$ep_store" "$ep_config"

    if printf '%s' "$ep_out" | grep -qi "requires OPENAI_API_KEY"; then
        bad "endpoint-override ignored - credential looked up against the config value, not the env"
    elif printf '%s' "$ep_out" | grep -qi "api ready\|state.:.opening\|listener_installed\|ready-signal"; then
        ok "credential resolved against the env-overridden endpoint (openai stored key wins over the groq config value)"
    elif printf '%s' "$ep_out" | grep -qi "no default input device\|no audio\|input device not found"; then
        warn "endpoint-override key resolved but no audio device (headless / muted)"
    else
        bad "endpoint-override worker startup shape not recognised (exit $ep_rc): $(printf '%s' "$ep_out" | head -c 300)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: in-process Rust runtime installs (VOICEPI_DICTATE_ENGINE=rust)
#
# Replaces the previous `whisper-dictate ui` probe, which could NEVER pass —
# it was structurally incapable of producing the evidence it grepped for, so
# it warn-skipped on every run regardless of whether the code was healthy:
#
#   1. `RuntimeSupervisor::start` is only reached from `start_runtime()`,
#      whose sole callers are user interactions (ui/tabs/compact.rs and
#      ui/tabs/shell.rs button handlers, plus restart-on-settings-save).
#      Nothing starts the runtime on launch, so a UI that is spawned and
#      SIGTERMed 3 s later without a single click never runs the branch.
#   2. Even when the branch does run, its message goes to
#      `RuntimeEvent::Stderr`, an in-process event channel that the UI
#      renders into its own log pane via `append_runtime_log`. It never
#      reaches process stdout/stderr, so grepping captured output cannot
#      observe it. Confirmed empirically: the UI writes 0 bytes.
#
# The supervisor's Phase B branch is already covered where it can actually be
# observed — at the event-channel level, by four dedicated Rust tests in
# `src/rust/tests/runtime_supervisor.rs` (`supervisor_phase_b_*`), which
# assert on the exact "Phase B in-process dispatch refused" string. The smoke
# script should cover what those tests cannot: that the shipped binary really
# installs the in-process runtime on this box, with this session's hotkey
# driver and permissions.
#
# `dictate-run --json-events` does exactly that, and its documented first-line
# contract is a stable `{"kind":"ready","ready":true,"engine":"rust",...}`
# envelope on real stdout — which is what the old grep pattern was reaching
# for. Run it bounded and gate on that envelope.
# --------------------------------------------------------------------------
section "in-process Rust runtime installs (dictate-run --json-events)"
if [ "$CMD_MODE" != "rust" ]; then
    warn "dictate-run is a Rust subcommand — not exposed by the Python fallback"
else
    # stdout and stderr are captured SEPARATELY. The first-line ready
    # envelope is a stdout contract, and stderr carries pre-ready chatter
    # that would otherwise win the `head -n 1` race: `load_settings()` prints
    # the parakeet migration warnings (config/load.rs) before the envelope,
    # and a failed Ctrl-C handler install warns from dictate_run.rs too.
    # Folding them together with `2>&1` would fail a perfectly healthy
    # runtime on any box that trips one of those paths.
    dictaterun_out="$(mktemp)"
    dictaterun_err="$(mktemp)"
    # 3 s is ample: the ready envelope is printed once the listener + sink
    # are installed, well under a second on this class of box. The verb runs
    # until Ctrl-C by design, so the SIGTERM is the expected end — gate on
    # the envelope, not on the exit status (`--preserve-status` surfaces the
    # signal as 143/15, which is success here, while a genuine startup
    # refusal exits non-zero on its own before the timeout fires).
    #
    # Nothing is injected: dictate-run only acts on a real PTT chord press,
    # and no keys are synthesised here.
    timeout --preserve-status --kill-after=1s 3s \
        whisper-dictate dictate-run --json-events \
        >"$dictaterun_out" 2>"$dictaterun_err"
    dictaterun_rc=$?
    dictaterun_first="$(head -n 1 "$dictaterun_out")"
    # Classify startup diagnostics across both streams.
    dictaterun_diag="$(cat "$dictaterun_out" "$dictaterun_err")"
    if printf '%s' "$dictaterun_first" | grep -q '"kind":"ready"' \
       && printf '%s' "$dictaterun_first" | grep -q '"engine":"rust"'; then
        # `dictate-run` builds its sink with the LENIENT
        # `rust_session_sink::build_production_sink`, which silently drops to
        # the PR 4 stub backends when real-backend construction fails (no
        # cached model, VOICEPI_WHISPER_MODEL_PATH unset, …) and still
        # reports ready. Reporting a green "runtime ready" there would claim
        # a transcribing binary that cannot transcribe. The degrade is
        # observable: the sink emits a RuntimeEvent::Stderr that
        # `--json-events` re-emits on stdout as
        # `{"kind":"stderr","line":"[rust-session] real backend init failed …"}`.
        #
        # Warn rather than fail, matching how the neighbouring sections
        # already classify the two things that trigger it — a missing model
        # (`self-test whisper-load`) and a missing feature (`self-test
        # audio-capture`) are warn-skips there too. What matters is that this
        # section stops claiming readiness it did not verify.
        #
        # The event only exists when the real-backend block was COMPILED IN.
        # Without `whisper-rs-local` the block is `#[cfg]`-ed out entirely and
        # the stubs are installed silently, leaving nothing to observe — so
        # the event check alone would fall straight through to `ok` on a
        # binary that cannot transcribe. Cover that build from the other side,
        # using the whisper-rs-local verdict `self-test whisper-load` recorded.
        # `unknown` deliberately does NOT trigger this: a whisper-load skip
        # for a missing model fixture says nothing about the feature, and
        # treating silence as absence would fire on every un-cached box.
        #
        # A build that cannot transcribe is a hard FAIL, not a skip. This
        # script is the release-verification step, and a binary that installs
        # its runtime but can never transcribe is exactly the thing a release
        # must not ship — letting the run exit 0 there would make the whole
        # smoke a rubber stamp. Building without the features is still fine as
        # a dev configuration; it just is not something this script can bless.
        #
        # The stub-fallback event covers the OTHER half, and its reason text
        # decides the verdict. `make_real_session()` rejects a build without
        # `audio-in-rust` with a message naming that feature — a build defect,
        # so fail. Every other reason (no cached model, an unresolvable model
        # path) is an environment condition the sibling sections already
        # warn-skip, so warn and print the reason.
        if grep -q "falling back to PR 4 stub backends" "$dictaterun_out"; then
            stub_reason="$(grep -o "real backend init failed ([^)]*)" "$dictaterun_out" | head -n 1)"
            if grep -q "audio-in-rust feature not compiled in" "$dictaterun_out"; then
                bad "runtime installs, but this build lacks audio-in-rust - the session runs on stub backends and cannot transcribe; rebuild with --features rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local"
            else
                warn "runtime installed but degraded to stub backends - cannot transcribe (${stub_reason:-reason not reported})"
            fi
        elif [ "$FEATURE_WHISPER_RS_LOCAL" = "no" ]; then
            bad "runtime installs, but this build lacks whisper-rs-local - the session runs on stub backends and cannot transcribe; rebuild with --features rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local"
        # A terminal audio failure during the window (mic disconnect, capture
        # callback error, resampler/VAD failure) stops the pump permanently
        # and re-emits `[rust-session-audio] device error` on stdout AFTER the
        # ready line. Ready-then-dead is not ready.
        elif grep -q "\[rust-session-audio\] device error" "$dictaterun_out"; then
            bad "audio pump died after install - no frames can reach the session: $(grep -o '\[rust-session-audio\] device error[^"]*' "$dictaterun_out" | head -c 200)"
        else
            # Surface the resolved driver + chord: on Wayland the driver MUST
            # be evdev (rdev's XRecord path is deaf there), so a silent flip
            # back to rdev is exactly the regression worth seeing here.
            dr_driver="$(printf '%s' "$dictaterun_first" | grep -oE '"driver":"[^"]+"' | cut -d: -f2 | tr -d '"')"
            dr_chord="$(printf '%s' "$dictaterun_first" | grep -oE '"chord":"[^"]+"' | cut -d: -f2 | tr -d '"')"
            ok "in-process Rust runtime ready (driver=${dr_driver:-?} chord=${dr_chord:-?})"
            if [ "$SESSION" = "wayland" ] && [ -n "$dr_driver" ] && [ "$dr_driver" != "evdev" ]; then
                bad "Wayland session resolved driver=$dr_driver - only evdev can observe keys under Wayland"
            fi
        fi
    elif printf '%s' "$dictaterun_diag" | grep -qi "rust-hotkeys\|rust-injection\|rebuild with"; then
        warn "dictate-run requires rust-hotkeys,rust-injection features (skipped on this build)"
    # Listener refusals: the WRAPPER TEXT CANNOT CLASSIFY THEM. Every
    # `InstallError::ListenerStartup` is rendered by dictate_run.rs:192 as
    #
    #   hotkey listener failed to start ({msg}); on Linux without an X display
    #   this is expected — … or use the evdev backend if you have
    #   `/dev/input/*` permissions
    #
    # so the string ALWAYS mentions both "X display" and "/dev/input",
    # whatever the real cause. Matching on either turns the branch into a
    # catch-all that silently downgrades genuine breakage — "manager thread
    # spawn failed", "evdev reader thread spawn failed", "listener thread did
    # not report readiness" — to a skip, letting a release-verification run
    # exit 0 on a runtime that cannot arm at all. (An earlier revision of this
    # section did exactly that in the other direction: it reported a headless
    # box as a permissions problem, because the hint mentions /dev/input.)
    #
    # So classify on things that are NOT the wrapper:
    #   - the inner message, for the one cause with distinctive wording
    #     (evdev's "no readable keyboard found under /dev/input … usermod -aG
    #     input $USER"), and
    #   - the SELECTED DRIVER for the headless case. "No display" is only an
    #     expected failure for rdev; evdev reads /dev/input and does not care
    #     about a display, so an evdev refusal on a headless box is a real
    #     failure. Empty DISPLAY/WAYLAND_DISPLAY does NOT prove rdev was
    #     picked: `resolve_driver` honours an explicit VOICEPI_HOTKEY_DRIVER
    #     override, and its `is_wayland_session()` returns true on
    #     XDG_SESSION_TYPE=wayland alone (hotkey/manager/mod.rs:220-250) —
    #     so a Wayland box with no WAYLAND_DISPLAY exported still selects
    #     evdev. Mirror that resolution here instead of guessing from the
    #     display variables.
    # Anything else is a hard failure.
    elif printf '%s' "$dictaterun_diag" | grep -qi "no readable keyboard\|usermod -aG input"; then
        warn "dictate-run: user lacks /dev/input access (add user to the 'input' group) - NOTE: backend capability was NOT verified, the sink's event stream is only drained after the listener installs"
    elif [ "$(resolve_hotkey_driver)" = "rdev" ] && [ -z "${DISPLAY:-}" ] \
         && printf '%s' "$dictaterun_diag" | grep -qi "listener failed to start"; then
        warn "dictate-run: rdev hotkey listener unavailable without a display (expected on headless / WSL) - NOTE: backend capability was NOT verified, the sink's event stream is only drained after the listener installs"
    elif [ "$dictaterun_rc" -eq 101 ]; then
        bad "dictate-run panicked: $(tail -n 3 "$dictaterun_err")"
    else
        bad "in-process Rust runtime did not report ready (exit $dictaterun_rc): $(printf '%s' "$dictaterun_diag" | head -c 300)"
    fi
    rm -f "$dictaterun_out" "$dictaterun_err"
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
