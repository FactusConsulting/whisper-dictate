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
        register|win_registerhotkey|wm_hotkey)
            # RegisterHotKey backend: Windows-only. `DriverKind::parse`
            # accepts the alias on every platform (so the env var is
            # parseable in the same way everywhere), but `spawn_register`
            # falls back to rdev on non-Windows targets and the actual
            # listener name in the install envelope is `rdev`. Report the
            # POST-fallback listener here so the smoke script's downstream
            # assertions match what the Rust binary would actually install.
            if [ "$(uname -s 2>/dev/null)" = "Linux" ] || [ "$(uname -s 2>/dev/null)" = "Darwin" ]; then
                echo "rdev"
            else
                echo "register"
            fi
            return
            ;;
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
    # Same logic for the RegisterHotKey aliases: `resolve_hotkey_driver`
    # returns `register` on non-Linux / non-Darwin (i.e. Git Bash on
    # Windows, which is where this script's `register` cases run for
    # real), and `rdev` on Linux/Darwin where `spawn_register` falls back
    # to rdev. Hard-coding `rdev` here would fail the self-test on Git
    # Bash — Codex P2 review of PR #650 (discussion_r3663290098).
    _uname_s="$(uname -s 2>/dev/null)"
    if [ "$_uname_s" = "Linux" ] || [ "$_uname_s" = "Darwin" ]; then
        REGISTER_FALLBACK=rdev
    else
        REGISTER_FALLBACK=register
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

    # -- Windows `RegisterHotKey` aliases -----------------------------------
    # `register` / `win_registerhotkey` / `wm_hotkey` all opt into the
    # Windows-only RegisterHotKey backend. On Linux / Darwin the spawn
    # shim falls back to rdev; on Git Bash the mirror keeps the `register`
    # name so the downstream `hotkey capture --driver register` assertion
    # matches the platform. Expectation is platform-derived (see
    # `$REGISTER_FALLBACK` above) — Codex P2 review of PR #650.
    _drv_case "$REGISTER_FALLBACK" "register"           "wayland"  "wayland-0"
    _drv_case "$REGISTER_FALLBACK" "win_registerhotkey" ""         ""
    _drv_case "$REGISTER_FALLBACK" "WM_HOTKEY"          "wayland"  ""
    _drv_case "$REGISTER_FALLBACK" "  Register  "       ""         ""

    if [ -z "$_drv_fails" ]; then
        ok "hotkey-driver resolution mirrors DriverKind::parse + resolve_driver (auto=$WAYLAND_AUTO, register=$REGISTER_FALLBACK on $(uname -s 2>/dev/null || echo unknown))"
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
# argparse-based flags in vp_cli.py) — a few flag-only checks — but NOT
# the Rust subcommands like `models list` or `config show`. Those
# sections warn-skip when only the Python fallback is available.
# --------------------------------------------------------------------------
CMD_SOURCE=""   # "installed" | "source" | "none"
CMD_MODE=""     # "rust" | "python"
CMD_ORIGIN=""   # "release" | "source-install" | "" (python fallback / none)

# --------------------------------------------------------------------------
# Classify an on-PATH `whisper-dictate` as a prebuilt RELEASE artifact or a
# locally built SOURCE install.
#
# Codex P2 #672 PRRT_kwDOSfNjQs6Ucarb cmt 3666625761: `CMD_SOURCE=installed`
# only says "a binary is on PATH" -- it does NOT say the binary is the shipped
# release artifact. `scripts/linux/install-rust-ui.sh:28-40` deliberately
# builds a source install with `--features audio-capture` ONLY, omitting the
# heavier `rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-local` set
# that `.github/workflows/release.yml:123` uses, because those pull in ONNX
# runtime + cmake/clang that a fresh box will not have. So a rebuild-with
# message from a source install is the DOCUMENTED, intentional feature skip,
# while the same message from a release artifact is a packaging regression.
# Sections that gate on "this is the shipping binary" must read CMD_ORIGIN,
# not CMD_SOURCE.
#
# Signals, cheapest first (all filesystem-only, no process launch):
#
#  * A binary invoked straight out of a cargo target dir (`target/release/`
#    or `target/debug/`) is a developer build by definition.
#  * `install-rust-ui.sh:48-53` installs a tiny shell WRAPPER at
#    `~/.local/bin/whisper-dictate` that does `export VOICEPI_APP_ROOT="<HERE>"`
#    and execs the real binary. `<HERE>` is the tree the installer ran from,
#    and installer lines 6-10/21-41 make the decision we mirror here: when
#    that tree carries `src/rust/Cargo.toml` AND has no prebuilt
#    `<HERE>/whisper-dictate`, the installer COMPILED the binary locally
#    (reduced feature set). When `<HERE>/whisper-dictate` exists it is the
#    unpacked release bundle and the installed binary is the shipped artifact.
#  * Anything else (a raw release binary on PATH, Homebrew's libexec wrapper
#    whose app root has no `src/rust`, nix, a distro package) is treated as a
#    release artifact -- the conservative default, so an unrecognised layout
#    still fails loudly on a missing-feature release rather than warn-skipping.
# --------------------------------------------------------------------------
classify_installed_origin() {
    cio_path="$1"

    case "$cio_path" in
        */target/release/whisper-dictate|*/target/debug/whisper-dictate)
            printf 'source-install\n'
            return 0
            ;;
    esac

    # Only text files can be the installer's wrapper; `grep -I` skips
    # binaries so we never sed a multi-megabyte ELF.
    cio_root=""
    if [ -f "$cio_path" ] && grep -Iq 'VOICEPI_APP_ROOT' "$cio_path" 2>/dev/null; then
        cio_root="$(sed -n 's/^export VOICEPI_APP_ROOT="\(.*\)"$/\1/p' "$cio_path" | head -n 1)"
    fi

    if [ -n "$cio_root" ] \
       && [ -f "${cio_root}/src/rust/Cargo.toml" ] \
       && [ ! -x "${cio_root}/whisper-dictate" ]; then
        printf 'source-install\n'
        return 0
    fi

    printf 'release\n'
}

detect_command() {
    if command -v whisper-dictate >/dev/null 2>&1; then
        CMD_SOURCE="installed"
        CMD_MODE="rust"
        CMD_ORIGIN="$(classify_installed_origin "$(command -v whisper-dictate)")"
    elif [ -d "${REPO_ROOT}/src/python/whisper_dictate" ] \
         && command -v python3 >/dev/null 2>&1; then
        CMD_SOURCE="source"
        CMD_MODE="python"
        CMD_ORIGIN=""
    else
        CMD_SOURCE="none"
        CMD_MODE=""
        CMD_ORIGIN=""
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
info "command origin     : ${CMD_ORIGIN:-(n/a)}"

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
# check that the audio subsystem is reachable. A missing device on a
# headless box is not a hard fail: the check downgrades to warn-skip
# so the smoke stays green on CI runners with no audio hardware.
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
# Used only by `self-test whisper-load`. The former Python `simulate-ptt`
# section — retired alongside the verb itself — built its faster-whisper
# model against a separate HuggingFace/CTranslate2 cache and never touched
# GGML; this fixture rule was the note flagging that. Kept here so anyone
# adding a new whisper-load-adjacent section understands what the fixture is.
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
# SECTION: simulate-session / dictate-mic (WAV + live-mic Rust CLI drivers)
#
# `simulate-ptt` was retired in favour of two Rust-native verbs: `simulate-
# session` (WAV-driven, cloud STT) and `dictate-mic` (live mic capture,
# cloud STT). Both need a cloud API key + network to drive a real end-to-end
# pipeline, so this smoke script — which must stay hermetic on a headless
# ThinkPad without a Groq/OpenAI key — only verifies the CLI surface is
# wired (`--help` exits 0 and prints the expected line). The real Rust
# in-process pipeline is exercised by the `simulate-session` job in
# `scripts/integration/groq-cli-smoke.sh` under the `GROQ_API_KEY` gate.
# --------------------------------------------------------------------------
section "simulate-session / dictate-mic (CLI surface --help check)"
if [ "$CMD_MODE" = "python" ]; then
    warn "simulate-session and dictate-mic are Rust subcommands — not exposed by the Python fallback"
else
    for verb in simulate-session dictate-mic; do
        if out="$(whisper-dictate "$verb" --help 2>&1)"; then
            if printf '%s' "$out" | grep -qi "$verb\|usage"; then
                ok "$verb --help exits 0 with usage output"
            else
                bad "$verb --help exit 0 but usage line not seen"
                info "$out"
            fi
        else
            rc=$?
            bad "$verb --help exit $rc"
            info "$out"
        fi
    done
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
    # Give this whole section its own push-to-talk ownership lock
    # (`hotkey::ptt_lock`). Three separate `hotkey capture` probes run
    # below (auto, --driver evdev, --driver register), and each one takes
    # ownership for the length of its window. Against the shared per-user
    # lock, a tray GUI running on the operator's desktop would refuse all
    # three, and the two --driver probes have no refusal classifier of
    # their own -- so the canonical smoke would exit non-zero while the
    # guard was working exactly as designed (Codex P2 #688).
    #
    # The guard's real two-process behaviour is exercised deliberately, in
    # its own section further down, with its own lock directory.
    hk_lock_dir="$(mktemp -d)"
    export VOICEPI_PTT_LOCK_DIR="$hk_lock_dir"
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
        # A running tray GUI (or a leftover `dictate-run`) legitimately owns
        # push-to-talk, and this verb is refused by design so the two cannot
        # both inject into the focused window. That is a working guard, not a
        # regression -- classify it before the generic patterns below, and
        # tell the operator what to do about it.
        if printf '%s' "$hk_out" | grep -qi "already owns push-to-talk\|REFUSED to register"; then
            warn "hotkey capture: push-to-talk is owned by another whisper-dictate process (quit it and re-run) - $(printf '%s\n' "$hk_out" | grep -o 'pid [0-9]*' | head -n 1)"
        # On Linux without evdev perms / X display / rust-hotkeys feature the
        # install refusal is expected. Only fail on unexpected shapes.
        elif printf '%s' "$hk_out" | grep -qi "rust-hotkeys\|permission\|evdev\|X display\|no display\|listener failed"; then
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

    # ---------------------------------------------------------------------
    # `--driver register` acceptance smoke: the Windows RegisterHotKey
    # backend cannot install on Linux (RegisterHotKey is a USER32 API and
    # Windows-only), but `DriverKind::parse` accepts the name on every
    # platform and `spawn_register` falls back to rdev with a diagnostic
    # line. This assertion pins the fallback contract: the flag must be
    # ACCEPTED (no "unknown driver" error), the install must not fail on
    # a working rdev platform, and the envelope must carry the rdev tag
    # (post-fallback). Regression bait: a future refactor that made
    # `--driver register` a hard error on Linux would silently break the
    # cross-platform default set in `whisper-dictate-gui::main` on
    # Windows, where the same env var is inspected by tests / support
    # scripts run under WSL.
    # ---------------------------------------------------------------------
    if whisper-dictate hotkey capture --help 2>&1 | grep -q -- "register"; then
        hk_reg_out="$(whisper-dictate hotkey capture --for 0.5 --driver register --json 2>&1)"
        hk_reg_rc=$?
        if [ "$hk_reg_rc" -eq 0 ]; then
            first_line="$(printf '%s\n' "$hk_reg_out" | head -n 1)"
            # On Linux the register spawn falls back to rdev; the
            # envelope must reflect the ACTUAL backend that installed.
            if printf '%s' "$first_line" | grep -q '"driver":"rdev"'; then
                ok "hotkey capture --driver register falls back to rdev cleanly on non-Windows"
            elif printf '%s' "$first_line" | grep -q '"driver":"win_registerhotkey"'; then
                # We're on Windows via WSL somehow; the real backend
                # took over. Also fine.
                ok "hotkey capture --driver register installed the RegisterHotKey backend"
            else
                warn "hotkey capture --driver register: unexpected envelope: $first_line"
            fi
        elif printf '%s' "$hk_reg_out" | grep -qi "rust-hotkeys\|permission\|no display\|listener failed"; then
            warn "hotkey capture --driver register: rdev fallback unavailable on this platform (expected without display/permissions/feature)"
        elif printf '%s' "$hk_reg_out" | grep -qi "unknown\|invalid\|unrecognised"; then
            bad "hotkey capture --driver register rejected as unknown - fallback contract broken"
        else
            bad "hotkey capture --driver register failed (exit $hk_reg_rc): $(printf '%s\n' "$hk_reg_out" | head -n 2)"
        fi
    else
        warn "hotkey capture --driver register alias not present in this build (pre-win-registerhotkey PR)"
    fi
    # Drop the per-section lock isolation so later sections see the real
    # per-user location again.
    unset VOICEPI_PTT_LOCK_DIR
    rm -rf "$hk_lock_dir"
fi

# --------------------------------------------------------------------------
# SECTION: single-owner push-to-talk guard (2026-07-29 interleaved-injection
# regression)
#
# A `dictate-run` CLI and the tray GUI both registered F9 on 2026-07-29. One
# press made both record, both transcribe, and both inject — the utterance
# came out written over itself, character by character, with nothing in
# either log. The guard (`hotkey::ptt_lock`) refuses the SECOND registration
# and says why.
#
# This section proves the shipped binary actually enforces it end to end,
# across two real processes, and — just as important — that quitting the
# first one hands ownership back. A guard that could strand a lock would be
# worse than the bug it replaces.
#
# The lock is redirected into a scratch directory so running this smoke
# script cannot disturb (or be disturbed by) a tray app the user has open.
# --------------------------------------------------------------------------
section "single-owner push-to-talk guard (two concurrent registrations)"
if [ "$CMD_MODE" = "python" ]; then
    warn "the push-to-talk ownership guard is Rust-side — not exposed by the Python fallback"
else
    ptt_lock_dir="$(mktemp -d)"
    ptt_first_out="$(mktemp)"
    # The install envelope marker. Named once because three separate checks
    # below key on it: the wait-for-holder loop, the "did it install at all"
    # gate, and the release re-check.
    ptt_installed='"kind":"listener_installed"'
    # Hold push-to-talk for a few seconds in the background, long enough for
    # the second registration below to collide with it.
    VOICEPI_PTT_LOCK_DIR="$ptt_lock_dir" \
        whisper-dictate hotkey capture --for 5 --json >"$ptt_first_out" 2>&1 &
    ptt_first_pid=$!
    # Wait for the holder to actually install before contending, rather than
    # sleeping a fixed amount and hoping. Bounded at ~5 s; give up early if
    # the holder died instead of installing.
    for _ in $(seq 1 50); do
        grep -q "$ptt_installed" "$ptt_first_out" 2>/dev/null && break
        kill -0 "$ptt_first_pid" 2>/dev/null || break
        sleep 0.1
    done

    if ! grep -q "$ptt_installed" "$ptt_first_out" 2>/dev/null; then
        wait "$ptt_first_pid" 2>/dev/null
        warn "push-to-talk guard: the first listener never installed on this box, so the collision cannot be exercised (see the hotkey capture section above)"
    else
        # Exit status is consumed by `if` directly: a successful second
        # registration IS the bug, so the happy path of the command is the
        # failure branch of the test.
        if ptt_second_out="$(VOICEPI_PTT_LOCK_DIR="$ptt_lock_dir" \
            whisper-dictate hotkey capture --for 0.5 --json 2>&1)"; then
            bad "push-to-talk guard: a SECOND process registered the same chord - this is the 2026-07-29 interleaved-injection bug"
        elif printf '%s' "$ptt_second_out" | grep -qi "already owns push-to-talk"; then
            # The refusal must be actionable: it has to name the pid to quit
            # and the corruption it prevented, or the user is left guessing
            # exactly as they were on 2026-07-29.
            if printf '%s' "$ptt_second_out" | grep -q "pid $ptt_first_pid"; then
                ok "push-to-talk guard: second registration refused, naming the holder (pid $ptt_first_pid)"
            elif printf '%s' "$ptt_second_out" | grep -qo "pid [0-9]*"; then
                ok "push-to-talk guard: second registration refused, naming a holder pid"
            else
                bad "push-to-talk guard: refused but did not name the holding process: $(printf '%s\n' "$ptt_second_out" | head -n 2)"
            fi
            if printf '%s' "$ptt_second_out" | grep -qi "interleav"; then
                ok "push-to-talk refusal explains the interleaved-injection consequence"
            else
                bad "push-to-talk refusal does not say what it prevented: $(printf '%s\n' "$ptt_second_out" | head -n 2)"
            fi
        else
            bad "push-to-talk guard: second registration failed for an unexpected reason: $(printf '%s\n' "$ptt_second_out" | head -n 2)"
        fi

        # Ownership must come back when the holder exits. A lock that
        # outlived its process would block every future launch.
        wait "$ptt_first_pid" 2>/dev/null
        ptt_third_out="$(VOICEPI_PTT_LOCK_DIR="$ptt_lock_dir" \
            whisper-dictate hotkey capture --for 0.3 --json 2>&1)"
        if printf '%s' "$ptt_third_out" | grep -q "$ptt_installed"; then
            ok "push-to-talk ownership is released when the holder exits"
        elif printf '%s' "$ptt_third_out" | grep -qi "already owns push-to-talk"; then
            bad "push-to-talk lock went STALE - it survived the holder's exit and now blocks every launch"
        else
            warn "push-to-talk release check inconclusive (listener would not reinstall): $(printf '%s\n' "$ptt_third_out" | head -n 1)"
        fi
    fi
    kill "$ptt_first_pid" 2>/dev/null
    rm -rf "$ptt_lock_dir"
    rm -f "$ptt_first_out"
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
# SECTION: self-test hotkey-boot (Windows PTT-boot regression — GUI wedge)
#
# End-to-end install of the Rust hotkey subsystem (rdev / evdev) with a
# fixed PTT chord. Fast, headless smoke: does NOT open the audio pump or
# load the Whisper model, only exercises the OS hook, driver selection,
# and coordinator wiring.
#
# What this catches (added after the Windows PTT bug where the GUI
# started with `VOICEPI_DICTATE_ENGINE=rust` but the chord fired no
# event — the GUI's `windows_subsystem = "windows"` had discarded every
# rdev-side error). On Linux / Wayland the same install path runs, so
# this section is a co-op smoke that would trip on a Linux-side
# regression to the shared install path.
#
# Restored per Codex P2 #642 (PRRT_kwDOSfNjQs6UKRsU): the earlier delete
# left this the only shell caller of `self-test hotkey-boot`, so a
# shared-install-path regression could again escape the integration
# run. We use `--chord ctrl_l` so the run doesn't depend on the
# operator's on-disk config.
#
# WHAT THIS CATCHES today, and what it does NOT:
#
# * Catches: `install_hotkey` returning an error along the shared code
#   path (missing feature gate, display refusal, missing device
#   permission, driver selection failure). Any Linux-side regression to
#   that path trips this section, and the same install path is what
#   the Windows GUI runs into first at startup.
#
# * Does NOT (Codex P2 #672 PRRT_kwDOSfNjQs6UZQ8Y): a listener thread
#   that installs cleanly but exits silently before the hold window
#   ends -- `BootSelfTestReport.listener_exited_early` is hardcoded
#   `false` on every success path (see
#   `src/rust/hotkey/boot_self_test.rs` L82-84: "Future refinement:
#   expose a `is_listener_alive()`"). Catching that class needs the
#   `is_listener_alive()` follow-up to land first.
#
# * Does NOT cover the Windows RegisterHotKey backend (Codex P2 #672
#   PRRT_kwDOSfNjQs6UZQ8I): this script is the Linux/Wayland smoke,
#   and `--driver auto` on Linux resolves to `evdev` (rdev on
#   X-forwarded builds). The Windows GUI sets
#   `VOICEPI_HOTKEY_DRIVER=register` at startup and, even under
#   `--driver register`, a modifier-only chord like `ctrl_l` is
#   intentionally routed back to rdev (see
#   `src/rust/hotkey/win_backend.rs`). So this section trips on
#   shared-code regressions but NOT on a Windows-specific register-
#   backend regression -- that needs a separate Windows-CI invocation
#   with a non-modifier-only chord, or the manual-test walkthrough in
#   `scripts/manual-test/README.md`.
# --------------------------------------------------------------------------
section "self-test hotkey-boot (Windows PTT-boot regression — same install path the GUI uses)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test is a Rust subcommand — not exposed by the Python fallback"
else
    hb_out="$(whisper-dictate self-test hotkey-boot --hold-ms 500 --chord ctrl_l --json 2>&1)"
    hb_rc=$?
    if [ "$hb_rc" -eq 0 ] && printf '%s' "$hb_out" | grep -q '"ok":true'; then
        # Report the driver so a future Wayland/X11 selector regression
        # (evdev vs rdev) surfaces in the smoke output.
        hb_driver="$(printf '%s' "$hb_out" | grep -o '"driver":"[^"]*"' | head -n 1)"
        ok "hotkey-boot install passed (${hb_driver:-driver=?})"
    elif printf '%s' "$hb_out" | grep -qi "rust-hotkeys\|rust-injection\|rebuild with"; then
        # Codex P2 #672 PRRT_kwDOSfNjQs6Uaj0I cmt 3665921401: a shipped
        # RELEASE binary is built by `.github/workflows/release.yml:123`
        # with both `rust-hotkeys` and `rust-injection`, so a rebuild-with
        # message from one means the shipped artifact is missing those
        # features -- a packaging regression that the smoke exists to
        # catch. Fall through to `bad` in that case only.
        #
        # Codex P2 #672 PRRT_kwDOSfNjQs6Ucarb cmt 3666625761: the gate is
        # `CMD_ORIGIN=release`, NOT merely `CMD_SOURCE=installed`. A
        # source install put on PATH by `scripts/linux/install-rust-ui.sh`
        # is also `installed`, yet that installer intentionally builds
        # with `--features audio-capture` alone (installer lines 28-40),
        # so its missing hotkey/injection features are the documented
        # expected skip -- failing on them would make the canonical smoke
        # unpassable on a supported install path. The Python dev fallback
        # (`CMD_SOURCE=source`) warn-skips for the same reason: neither
        # path ever claimed to be the shipping binary.
        if [ "$CMD_SOURCE" = "installed" ] && [ "$CMD_ORIGIN" = "release" ]; then
            bad "hotkey-boot FAILED: installed release binary is missing rust-hotkeys / rust-injection features -- packaging regression: $(printf '%s\n' "$hb_out" | head -n 1)"
        else
            warn "self-test hotkey-boot requires rust-hotkeys,rust-injection features (skipped on this ${CMD_ORIGIN:-$CMD_SOURCE} build)"
        fi
    elif printf '%s' "$hb_out" | grep -q "ListenerStartup\|no X display\|permission\|no readable keyboard\|usermod -aG input\|MissingDisplayError"; then
        # On non-Windows: a headless / no-display box legitimately fails
        # install here and it is an environment gap, not a regression. On
        # Windows (Codex P2 #672 PRRT_kwDOSfNjQs6UZY7Q): a permission
        # refusal from the RegisterHotKey backend IS a regression -- the
        # tray runs with per-user permissions and RegisterHotKey does not
        # need elevated ones, so a "permission" error at boot means the
        # backend actually failed. Fall through to `bad` in that case
        # so the release gate trips.
        #
        # Match set:
        # * `ListenerStartup` / `no X display` -- rdev pathways under X.
        # * `permission` -- generic Linux permission text.
        # * `no readable keyboard` / `usermod -aG input` -- evdev's actual
        #   permission refusal when the user is not in the `input` group
        #   (Codex P2 #672 PRRT_kwDOSfNjQs6UZY9m cmt 3665545676). Same
        #   wording the later `dictate-run` smoke matches, so a Wayland
        #   auto-evdev install without input-group membership produces a
        #   warn on the shared Linux path rather than a false-bad.
        # * `MissingDisplayError` -- rdev's actual serialized error on
        #   headless Linux / WSL when auto selects rdev and no X display
        #   exists (Codex P2 #672 PRRT_kwDOSfNjQs6UZ5Bd cmt 3665819810).
        #   rdev formats its error via `format!("{err:?}")`
        #   (rdev_driver.rs:377), and `InstallError::ListenerStartup`
        #   wraps it as `rdev listener failed to start: MissingDisplayError`.
        #   That string contains neither `ListenerStartup` (the
        #   enum-variant name) nor `no X display` (rdev never emits that
        #   literal), so without the missing-display token a headless
        #   install falls through to `bad`.
        #
        #   Codex P2 #672 PRRT_kwDOSfNjQs6Uaj0A cmt 3665921394:
        #   deliberately do NOT match the generic
        #   `rdev listener failed to start` wrapper -- that string
        #   prefixes EVERY rdev listener-startup failure (see
        #   `InstallError::ListenerStartup` in `src/rust/hotkey/mod.rs:191`),
        #   so a future non-headless rdev regression (permission denied,
        #   OS refusal, etc.) would silently downgrade to `warn` and let
        #   the release ship. Only the specific `MissingDisplayError`
        #   token identifies the genuine headless environment gap.
        case "$(uname -s 2>/dev/null || echo unknown)" in
            MINGW*|MSYS*|CYGWIN*|Windows_NT)
                bad "hotkey-boot FAILED on Windows: permission/listener refusal is a regression, not an environment gap: $(printf '%s\n' "$hb_out" | head -n 1)"
                ;;
            *)
                warn "hotkey-boot: listener refused (missing display / permissions — expected on headless non-Windows): $(printf '%s\n' "$hb_out" | head -n 1)"
                ;;
        esac
    else
        bad "hotkey-boot FAILED — install-path regression (this is the class of bug that broke Windows PTT in the GUI): $(printf '%s\n' "$hb_out" | tail -n 3)"
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
# SECTION: corpus-record (Linux installer audio-capture regression — #629)
#
# PR #629 removed the Python `vp_corpus_record.py` fallback, so on a build
# WITHOUT `--features audio-capture` the CLI compiles to a stub that prints
# "corpus-record is not available in this build: rebuild with
# `--features audio-capture`" and exits non-zero. Every shipping release
# builds with the feature; the Linux source installer
# (`scripts/linux/install-rust-ui.sh`) originally shipped WITHOUT it, which
# is the codex P1 this smoke section defends against.
#
# Two checks:
#   1. `corpus-record --help` prints clap usage and exits 0 — cheap CLI
#      surface check (clap doesn't run the handler, so a stub build passes
#      this one too; kept as the "the verb is at least wired" smoke).
#   2. `corpus-record <bogus id>` — a well-formed id that isn't in any
#      manifest. On a feature-on build the recorder is invoked, fails to
#      resolve the id, and emits a `corpus_record_error` JSON event with
#      exit 0. On a stub build the dispatch stub returns the "rebuild with
#      `--features audio-capture`" anyhow error, main.rs prints it to
#      stderr, and the process exits non-zero. The section fails ONLY
#      when the stub phrase appears — anything else (audio device missing,
#      manifest lookup failure) is expected/acceptable.
# --------------------------------------------------------------------------
section "corpus-record (Linux installer audio-capture regression — #629)"
if [ "$CMD_MODE" = "python" ]; then
    warn "corpus-record is a Rust subcommand — not exposed by the Python fallback"
else
    cr_help_out="$(whisper-dictate corpus-record --help 2>&1)"
    cr_help_rc=$?
    if [ "$cr_help_rc" -eq 0 ] && printf '%s' "$cr_help_out" | grep -qi "corpus-record\|usage"; then
        # A stub build still passes clap's --help (the stub is in the handler,
        # not in the parser), but the --help output itself must NEVER include
        # the stub's "rebuild with `--features audio-capture`" phrase. This
        # catches the accidental case where someone leaks the stub message
        # into the doc comment / long-help.
        if printf '%s' "$cr_help_out" | grep -q "rebuild with .--features audio-capture."; then
            bad "corpus-record --help leaks the audio-capture stub message"
            info "$(printf '%s\n' "$cr_help_out" | head -n 5)"
        else
            ok "corpus-record --help works (verb wired, no stub leak in usage)"
        fi
    else
        bad "corpus-record --help failed (exit $cr_help_rc): $(printf '%s\n' "$cr_help_out" | head -n 2)"
    fi

    # Bogus but well-formed id — passes `is_safe_corpus_id` so the recorder
    # dispatch runs, then fails to resolve. On a feature-on build the failure
    # is "not in the corpus manifest" (exit 0, JSON event on stdout); on a
    # stub build the failure is "rebuild with `--features audio-capture`"
    # (non-zero exit, message on stderr). We look for the stub phrase; any
    # other failure shape is fine for this smoke.
    cr_out="$(whisper-dictate corpus-record wd-smoke-nonexistent 2>&1)"
    if printf '%s' "$cr_out" | grep -q "rebuild with .--features audio-capture."; then
        bad "corpus-record was built WITHOUT --features audio-capture — this is the #629 installer regression"
        info "$(printf '%s\n' "$cr_out" | head -n 3)"
    else
        ok "corpus-record dispatches to the native recorder (audio-capture feature is compiled in)"
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
# SECTION: self-test feedback (Round 2/3 backend — PTT audible cues)
#
# Exercises the same SystemCueSink the live session uses at PTT press +
# release. Reports which backend the resolver picked (kernel32_beep /
# paplay / pw-play / noop). The verb intentionally fails only when the
# env gate is on but no backend is available (the silent-mute regression);
# with the gate off it exits 0 and reports backend="noop" — which is the
# correct "user did not opt in" answer.
# --------------------------------------------------------------------------
section "self-test feedback (Round 2/3 — PTT audible cues)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test feedback is a Rust subcommand — not exposed by the Python fallback"
else
    fb_out="$(whisper-dictate self-test feedback --delay-ms 50 --json 2>&1)"
    fb_rc=$?
    if [ "$fb_rc" -eq 0 ] && printf '%s' "$fb_out" | grep -q '"ok":true'; then
        fb_backend="$(printf '%s' "$fb_out" | grep -oE '"backend":"[^"]+"' | cut -d: -f2 | tr -d '"')"
        ok "feedback cues: backend=$fb_backend (start+stop played)"
    else
        bad "self-test feedback FAILED — cues silently muted: $(printf '%s\n' "$fb_out" | tail -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: self-test audio-ducking (Round 2/3 backend — WASAPI ducker)
#
# WASAPI-only backend today — on Linux the verb reports
# backend="unsupported_platform" and exits 0. Failure only when the env
# gate is on but no backend is available (the silent-no-duck regression).
# --------------------------------------------------------------------------
section "self-test audio-ducking (Round 2/3 — WASAPI ducker)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test audio-ducking is a Rust subcommand — not exposed by the Python fallback"
else
    ad_out="$(whisper-dictate self-test audio-ducking --duration-ms 200 --json 2>&1)"
    ad_rc=$?
    if [ "$ad_rc" -eq 0 ] && printf '%s' "$ad_out" | grep -q '"ok":true'; then
        ad_backend="$(printf '%s' "$ad_out" | grep -oE '"backend":"[^"]+"' | cut -d: -f2 | tr -d '"')"
        ok "audio-ducking: backend=$ad_backend (enter+exit fired)"
    else
        bad "self-test audio-ducking FAILED: $(printf '%s\n' "$ad_out" | tail -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: self-test profile-match (Round 2/3 backend — target profiles)
#
# Runs the user's live profile list against a synthetic Cursor window.
# `matched=false` is a valid diagnostic answer (the operator has no
# Cursor profile configured); we only trip on a config-load error.
# --------------------------------------------------------------------------
section "self-test profile-match (Round 2/3 — target profiles)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test profile-match is a Rust subcommand — not exposed by the Python fallback"
else
    pm_out="$(whisper-dictate self-test profile-match --title "Cursor" --process "cursor" --json 2>&1)"
    pm_rc=$?
    if [ "$pm_rc" -eq 0 ] && printf '%s' "$pm_out" | grep -q '"ok":true'; then
        pm_matched="$(printf '%s' "$pm_out" | grep -oE '"matched":(true|false)' | cut -d: -f2)"
        ok "profile-match: synthetic Cursor window matched=$pm_matched"
    else
        bad "self-test profile-match FAILED: $(printf '%s\n' "$pm_out" | tail -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: self-test history-write (Round 2/3 backend — history JSONL sink)
#
# Writes one synthetic utterance event through the shipping
# history_sink_from_settings and reports the file path + bytes written.
# Honours the operator's config gate: `enabled=false` means history is
# disabled, and the verb still exits 0 (that's the correct "user did not
# opt in" answer).
# --------------------------------------------------------------------------
section "self-test history-write (Round 2/3 — history JSONL sink)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test history-write is a Rust subcommand — not exposed by the Python fallback"
else
    hw_out="$(whisper-dictate self-test history-write --text "wayland smoke" --json 2>&1)"
    hw_rc=$?
    if [ "$hw_rc" -eq 0 ] && printf '%s' "$hw_out" | grep -q '"ok":true'; then
        hw_enabled="$(printf '%s' "$hw_out" | grep -oE '"enabled":(true|false)' | head -1 | cut -d: -f2)"
        ok "history-write: enabled=$hw_enabled (path resolved)"
    else
        bad "self-test history-write FAILED: $(printf '%s\n' "$hw_out" | tail -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: self-test metrics-write (Round 2/3 backend — metrics JSONL sink)
#
# Same shape as history-write but for the metrics sink. The default gate
# (json_output off, metrics_jsonl unset) reports enabled=false and passes.
# --------------------------------------------------------------------------
section "self-test metrics-write (Round 2/3 — metrics JSONL sink)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test metrics-write is a Rust subcommand — not exposed by the Python fallback"
else
    mw_out="$(whisper-dictate self-test metrics-write --text "wayland smoke" --json 2>&1)"
    mw_rc=$?
    if [ "$mw_rc" -eq 0 ] && printf '%s' "$mw_out" | grep -q '"ok":true'; then
        mw_enabled="$(printf '%s' "$mw_out" | grep -oE '"enabled":(true|false)' | head -1 | cut -d: -f2)"
        ok "metrics-write: enabled=$mw_enabled (gate honoured)"
    else
        bad "self-test metrics-write FAILED: $(printf '%s\n' "$mw_out" | tail -n 3)"
    fi
fi

# --------------------------------------------------------------------------
# SECTION: self-test preview (Round 2/3 backend — live partial transcribe)
#
# Boots a real PreviewEngine with a canned mock backend, pushes 5 fake
# frames, and asserts at least one emission lands on the sink. Fails on
# an empty emission list (worker thread / channel wiring broken).
# --------------------------------------------------------------------------
section "self-test preview (Round 2/3 — live partial transcribe)"
if [ "$CMD_MODE" = "python" ]; then
    warn "self-test preview is a Rust subcommand — not exposed by the Python fallback"
else
    pv_out="$(whisper-dictate self-test preview --json 2>&1)"
    pv_rc=$?
    if [ "$pv_rc" -eq 0 ] && printf '%s' "$pv_out" | grep -q '"ok":true'; then
        pv_count="$(printf '%s' "$pv_out" | grep -oE '"emissions":\[[^]]*\]' | grep -oE '"text"' | wc -l)"
        ok "preview: $pv_count emissions collected"
    else
        bad "self-test preview FAILED — engine did not emit: $(printf '%s\n' "$pv_out" | tail -n 3)"
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
# SECTION: dictate engine dispatch (Phase 1 default flip — Rust is default)
#
# Audit item 5 Phase A step 2 + Phase 1 default flip. Three checks:
#
#   1. Default (unset env) resolves to `rust` on both dispatchers (Python
#      `select_engine` + Rust `EngineChoice`). Pins the flip so a
#      revert-to-Python-default surfaces here.
#   2. Explicit `VOICEPI_DICTATE_ENGINE=python` safety-valve opt-out still
#      resolves to `python` — the transition-window escape hatch retired
#      only in the Phase 2 PR.
#   3. The Rust `dictate-run --help` verb is reachable — pins that the
#      Rust runtime the flip depends on is at least wired up in the shipped
#      binary. The `dictate-run` verb has its own section above; here we
#      only cross-check that the flip is defensible on this build.
#
# The full PTT loop is manual QA (needs a display + audio + a running Rust
# binary with the required features); this section only exercises the
# dispatch layer.
# --------------------------------------------------------------------------
section "dictate engine dispatch (Phase 1 default flip — Rust is default)"
if [ "$CMD_MODE" = "python" ] || command -v python3 >/dev/null 2>&1; then
    # Check 1: default (unset) → rust.
    engine_default_out="$(env -u VOICEPI_DICTATE_ENGINE PYTHONPATH="${REPO_ROOT}/src/python" python3 -c '
from whisper_dictate.vp_dictate_engine import (
    ENGINE_ENV, ENGINE_PYTHON, ENGINE_RUST, select_engine,
)
picked = select_engine()
assert picked == ENGINE_RUST, (
    "Phase 1 default flip regressed: unset %s must resolve to rust "
    "(got %r) -- if this failed the whole flip is broken"
    % (ENGINE_ENV, picked)
)
print("default=%s (unset->%s)" % (picked, ENGINE_RUST))
' 2>&1)"
    if [ $? -eq 0 ]; then
        ok "Python runtime default (unset) resolves to rust ($engine_default_out)"
    else
        bad "Phase 1 default flip broken on Python side: $(printf '%s\n' "$engine_default_out" | head -2)"
    fi

    # Check 2: explicit `python` opt-out still works.
    engine_optout_out="$(VOICEPI_DICTATE_ENGINE=python PYTHONPATH="${REPO_ROOT}/src/python" python3 -c '
from whisper_dictate.vp_dictate_engine import (
    ENGINE_ENV, ENGINE_PYTHON, select_engine,
)
picked = select_engine()
assert picked == ENGINE_PYTHON, (
    "safety-valve opt-out broken: %s=python must resolve to python "
    "(got %r) -- operators cannot fall back if this regresses"
    % (ENGINE_ENV, picked)
)
print("opt-out=%s" % picked)
' 2>&1)"
    if [ $? -eq 0 ]; then
        ok "VOICEPI_DICTATE_ENGINE=python safety-valve opt-out works ($engine_optout_out)"
    else
        bad "safety-valve opt-out broken: $(printf '%s\n' "$engine_optout_out" | head -2)"
    fi

    # Check 3: explicit `rust` still works.
    engine_explicit_out="$(VOICEPI_DICTATE_ENGINE=rust PYTHONPATH="${REPO_ROOT}/src/python" python3 -c '
from whisper_dictate.vp_dictate_engine import (
    ENGINE_ENV, ENGINE_RUST, select_engine,
)
picked = select_engine()
assert picked == ENGINE_RUST, (
    "explicit %s=rust must resolve to rust (got %r)"
    % (ENGINE_ENV, picked)
)
print("explicit-rust=%s" % picked)
' 2>&1)"
    if [ $? -eq 0 ]; then
        ok "explicit VOICEPI_DICTATE_ENGINE=rust works ($engine_explicit_out)"
    else
        bad "explicit rust dispatch broken: $(printf '%s\n' "$engine_explicit_out" | head -2)"
    fi
else
    warn "engine dispatch verify needs python3 in PATH (Rust-only build)"
fi

# --------------------------------------------------------------------------
# SECTION: Rust dictate-run reachable (Phase 1 flip prereq)
#
# The Phase 1 default (unset env → Rust) relies on the Rust binary
# exposing `dictate-run`. If it disappeared, every fresh install would
# still fall back to the Python engine (or a hard error) — smokes as
# green but the flip's whole reason is defeated. Pin the verb's --help
# so a drop of the CLI surface trips this check.
# --------------------------------------------------------------------------
section "Rust dictate-run --help (Phase 1 default flip prereq)"
if [ "$CMD_MODE" = "python" ]; then
    warn "dictate-run is a Rust subcommand — not exposed by the Python fallback"
elif dr_flip_out="$(whisper-dictate dictate-run --help 2>&1)"; then
    if printf '%s' "$dr_flip_out" | grep -q -- '--json-events'; then
        ok "dictate-run --help reachable — Phase 1 default flip prereq satisfied"
    else
        bad "dictate-run --help exit 0 but --json-events flag missing from usage"
    fi
else
    bad "dictate-run --help failed — Phase 1 default flip has no runtime to dispatch to"
    info "$(printf '%s\n' "$dr_flip_out" | head -n 3)"
fi

# --------------------------------------------------------------------------
# SECTION: `whisper-dictate run --help` under the safety-valve opt-out
#
# Verifies the safety-valve opt-out (`VOICEPI_DICTATE_ENGINE=python`)
# doesn't crash the CLI surface. `--help` short-circuits in Python
# argparse before any dispatch decision, so this is a cheap "the env
# var itself doesn't break the run verb" smoke — not a real dispatch
# exercise. Timeout guards against a runaway process on boxes where
# the run verb needs Python bootstrapping (CUDA DLLs, HF cache) that
# might stall waiting for a mic / display.
# --------------------------------------------------------------------------
section "run --help under VOICEPI_DICTATE_ENGINE=python opt-out"
if [ "$CMD_MODE" = "python" ]; then
    warn "run --help under opt-out needs the installed Rust binary (skipped on Python fallback)"
else
    optout_help_out="$(VOICEPI_DICTATE_ENGINE=python timeout 15 whisper-dictate run --help 2>&1)"
    optout_help_rc=$?
    if [ "$optout_help_rc" -eq 0 ] \
       && printf '%s' "$optout_help_out" | grep -qi "usage\|--key\|--mode"; then
        ok "run --help reachable with VOICEPI_DICTATE_ENGINE=python set"
    elif [ "$optout_help_rc" -eq 124 ] || [ "$optout_help_rc" -eq 137 ]; then
        # timeout(1): 124 = expired, 128+9 = SIGKILL after --kill-after.
        # The Python worker started but didn't reach --help output in
        # the window — could be a CUDA DLL bootstrap or HF cache warm-up.
        # Not a smoke failure, just less informative than we hoped.
        warn "run --help timed out under opt-out (Python worker slow to boot; not a flip regression)"
    else
        bad "run --help failed under VOICEPI_DICTATE_ENGINE=python opt-out (exit $optout_help_rc)"
        info "$(printf '%s\n' "$optout_help_out" | head -n 3)"
    fi
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
# This check pins the fix by saving the key under the GROQ account,
# leaving the config's endpoint unset (so the OPENAI default applies), and
# overriding to Groq via env. The worker must find the groq-stored key --
# if it classifies the default OpenAI endpoint instead, there is nothing to
# find and it dies at startup with the missing-key message.
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
    # GROQ credential only, and the env override below points at Groq while
    # the config leaves `stt_base_url` unset -- which `AppSettings::default`
    # fills with the OPENAI url (config/settings.rs:93). The two endpoints
    # must DIVERGE or the check proves nothing: with an OpenAI override the
    # default and the override coincide, and the pre-fix implementation that
    # classified the config value would pass just as happily.
    printf '{"stt-api-key:groq":"smoke-groq-not-a-real-key"}\n' >"$ep_store"
    # `stt_base_url` is deliberately ABSENT from the scratch config.
    # `runtime_setting_value` resolves the config value BEFORE the process
    # environment (config/schema.rs:131-138), so a config that pins the URL
    # wins over the `VOICEPI_STT_BASE_URL` set below -- `worker_env_overrides`
    # would bake the config's value into `command.env` and the override this
    # section exists to exercise would never take effect. The check would then
    # fail against a correct implementation, for a reason that has nothing to
    # do with credential lookup. Omitting the key is also the real scenario:
    # a user overriding the endpoint from the shell has not written it to
    # their config.
    printf '{"stt_backend":"openai","stt_model":"whisper-large-v3-turbo","post_processor":"off"}\n' >"$ep_config"

    ep_out="$(env -u VOICEPI_STT_API_KEY -u VOICEPI_POST_API_KEY \
                  -u GROQ_API_KEY -u OPENAI_API_KEY \
                  VOICEPI_API_KEY_STORE="$ep_store" \
                  VOICEPI_DISABLE_OS_KEYRING=1 \
                  VOICEPI_CONFIG="$ep_config" \
                  VOICEPI_STT_BASE_URL=https://api.groq.com/openai/v1 \
              timeout --preserve-status --kill-after=2s 12s \
              whisper-dictate run 2>&1)"
    ep_rc=$?
    rm -f "$ep_store" "$ep_config"

    if printf '%s' "$ep_out" | grep -qi "requires OPENAI_API_KEY"; then
        bad "endpoint-override ignored - credential looked up against the config value, not the env"
    elif printf '%s' "$ep_out" | grep -qi "api ready\|state.:.opening\|listener_installed\|ready-signal"; then
        ok "credential resolved against the env-overridden endpoint (groq stored key wins over the openai default)"
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
    # A tray GUI (or another dictate-run) already owns push-to-talk, so this
    # one is refused by design (`hotkey::ptt_lock`). Working guard, not a
    # regression -- classify it ahead of the generic listener failures.
    elif printf '%s' "$dictaterun_diag" | grep -qi "already owns push-to-talk\|REFUSED to register"; then
        warn "in-process runtime: push-to-talk is owned by another whisper-dictate process (quit it and re-run) - $(printf '%s' "$dictaterun_diag" | grep -o 'pid [0-9]*' | head -n 1)"
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
# SECTION: postprocess prompt preserves the spoken language (#685)
#
# User-reported bug: dictating "1 2 3 4 5 6" in Danish with lang=da and
# post_mode=clean came back as English "One, two, three, four, five, six" --
# the LLM cleanup prompt never told the model which language to answer in,
# nor to leave numerals alone. The fix threads the configured `lang` into
# `build_prompt` on BOTH the Rust and the Python path.
#
# The LLM's answer cannot be asserted deterministically, so this checks the
# PROMPT CONTRACT instead: the hidden `postprocess` verb's `build_prompt`
# action must emit the preserve-language + preserve-numerals instructions.
# No network, no model -- pure string construction.
# --------------------------------------------------------------------------
section "postprocess prompt preserves the spoken language (#685)"
if [ "$CMD_MODE" = "python" ]; then
    warn "postprocess build_prompt is a Rust subcommand — not exposed by the Python fallback"
else
    pp_payload='{"action":"build_prompt","text":"1, 2, 3, 4, 5, 6","mode":"clean","lang":"da"}'
    pp_out="$(printf '%s' "$pp_payload" | whisper-dictate postprocess 2>&1)"
    pp_rc=$?
    if [ "$pp_rc" -ne 0 ]; then
        bad "postprocess build_prompt FAILED (exit $pp_rc): $(printf '%s\n' "$pp_out" | tail -n 3)"
    elif ! printf '%s' "$pp_out" | grep -q 'the input is in da (ISO 639-1 code)'; then
        bad "postprocess prompt does not name the configured language: $(printf '%s' "$pp_out" | head -c 300)"
    elif ! printf '%s' "$pp_out" | grep -q 'Never translate the text or switch to another language'; then
        bad "postprocess prompt does not forbid translation: $(printf '%s' "$pp_out" | head -c 300)"
    elif ! printf '%s' "$pp_out" | grep -q 'do not convert digits into words or words into digits'; then
        bad "postprocess prompt does not pin numerals: $(printf '%s' "$pp_out" | head -c 300)"
    else
        ok "postprocess prompt (lang=da, mode=clean): language pinned, translation forbidden, numerals preserved"
    fi

    # Unset lang (auto-detect) must NOT license a translation either.
    pp_auto='{"action":"build_prompt","text":"1, 2, 3","mode":"clean","lang":""}'
    pp_auto_out="$(printf '%s' "$pp_auto" | whisper-dictate postprocess 2>&1)"
    if printf '%s' "$pp_auto_out" | grep -q 'reply in the same language as the input'; then
        ok "postprocess prompt (lang unset): reply still bound to the input language"
    else
        bad "postprocess prompt with unset lang does not bind the reply language: $(printf '%s' "$pp_auto_out" | head -c 300)"
    fi
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
