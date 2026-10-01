use clap::Subcommand;

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum HotkeyCommand {
    /// Install the PTT listener for a bounded window and print every OS key
    /// event and chord match/release it observes. Exits 0 when the window
    /// elapses (or immediately when `--exit-on-chord` is set and the
    /// configured chord fires). A listener startup failure (no display,
    /// missing accessibility permission, unsupported chord) exits non-zero
    /// with a clear message so smoke scripts can distinguish "listener
    /// unavailable on this platform" from a genuine regression.
    ///
    /// Output line prefix is `[hotkey-capture]`. `--json` switches to one
    /// JSON object per line (JSONL): `{"t":<seconds>,"kind":"...","...":...}`.
    /// Chord match/release/cancel objects include nullable `focused`: it is
    /// `true` or `false` for the GUI's focus-aware diagnostic and `null` for
    /// ordinary capture or when the platform cannot attribute focus.
    /// The plain-text format is stable-ish (line prefix + kind tokens) but
    /// the JSON keys are the contract callers should pin against.
    Capture {
        /// Duration in seconds to keep the listener installed. Fractional
        /// values are allowed so smoke scripts can use a sub-second window
        /// (e.g. `--for 0.5`). Parsed as `f64` in the handler; the string
        /// carrier keeps the enum `Eq`-derivable so parse-shape tests can
        /// still `assert_eq!` variants without a bespoke matcher.
        #[arg(long = "for", value_name = "SECONDS", default_value = "5")]
        for_secs: String,
        /// Emit machine-readable JSONL instead of the human-readable
        /// `[hotkey-capture] ...` lines.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Emit listener + chord lifecycle events only. This internal mode is
        /// used by the GUI verifier so unrelated physical keys never cross
        /// its subprocess pipe.
        #[arg(long = "chord-events-only", default_value_t = false, hide = true)]
        chord_events_only: bool,
        /// Stamp chord lifecycle events with whether this process owned the
        /// foreground window at the event source. Internal GUI-verifier IPC.
        #[arg(long = "focus-process", value_name = "PID", hide = true)]
        focus_process: Option<u32>,
        /// Exit 0 as soon as the configured PTT chord fires. Useful for CI
        /// smoke tests where a driven synthetic press proves the whole path.
        #[arg(long = "exit-on-chord", default_value_t = false)]
        exit_on_chord: bool,
        /// Capture the first complete supported chord and offer to save it to
        /// the selected config file after the user confirms it.
        #[arg(long = "configure", alias = "save", default_value_t = false)]
        configure: bool,
        /// Override the config file path used to look up the PTT chord.
        /// Default: platform user config (honours `VOICEPI_CONFIG` when this
        /// flag is omitted).
        #[arg(long, value_name = "PATH")]
        config: Option<String>,
        /// Force a specific OS listener: `auto` (default; picks evdev on
        /// Linux Wayland, rdev everywhere else), `rdev` (X11 / Windows /
        /// macOS global hook), `evdev` (Linux `/dev/input`, the only
        /// Wayland-capable backend), or `register` (Windows-only
        /// RegisterHotKey backend that bypasses the WH_KEYBOARD_LL hook
        /// chain). `x11`/`wayland` are accepted aliases for the first
        /// two; `win_registerhotkey` / `wm_hotkey` are aliases for
        /// `register`. On non-Windows the `register` name is accepted
        /// but falls back to rdev with a diagnostic line.
        ///
        /// Setting this flag sets `VOICEPI_HOTKEY_DRIVER` for the process
        /// so the same selection logic the runtime uses fires here. Reject
        /// unrecognised values BEFORE installing anything so a typo does
        /// not silently fall back.
        #[arg(long, value_name = "AUTO|RDEV|EVDEV|REGISTER", default_value = "auto")]
        driver: String,
        /// Chord to listen for, instead of the one in the config
        /// (`ctrl_l`, `shift_r+f9`, ...). Same `+`-separated syntax and key
        /// names as `settings.key`.
        ///
        /// Exists so a chord can be verified WITHOUT editing the user's real
        /// config: confirming "does this key reach the listener on Wayland"
        /// previously meant mutating `config.json`, restoring it afterwards,
        /// and hoping nothing crashed in between.
        #[arg(long, value_name = "CHORD")]
        chord: Option<String>,
    },
}
