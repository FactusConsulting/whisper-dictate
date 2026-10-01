use clap::Subcommand;

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SelfTestCommand {
    /// Exercises the self-injection guard and tracker end-to-end with
    /// synthetic events - NO OS-level hooks, NO
    /// audio, NO display - so the check runs on any CI container.
    ///
    /// Each iteration simulates: first PTT chord -> guard-bracketed
    /// injection burst (unmapped VKs + STALE_MODIFIER_VKS-shaped releases)
    /// -> second PTT chord. Fails if any injected event leaks past the
    /// guard OR the second chord silently doesn't fire (the classic wedge
    /// symptom).
    ///
    /// Feature-gated behind `rust-hotkeys` + `rust-injection` - a stock
    /// build exits with an actionable "rebuild with --features ..."
    /// message rather than hanging or reporting a false pass.
    PttWedge {
        /// Iterations to run. Each iteration is independent (fresh guard
        /// and tracker); the loop stops at the first failure so the report
        /// unambiguously names the broken cycle.
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        /// Emit a single JSON object with `kind`, `driver`, `iterations`,
        /// `all_passed`, and per-iteration `results` - the machine-
        /// readable contract callers should pin.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Which listener path the test reports it exercised. Accepts the
        /// same canonical names and aliases (`x11` / `wayland`) as the
        /// `hotkey capture --driver` flag. The self-test drives the shared
        /// guard + tracker filter both drivers converge on, so this flag
        /// only affects the report label - the underlying assertions are
        /// identical.
        #[arg(long, value_name = "AUTO|RDEV|EVDEV", default_value = "auto")]
        driver: String,
    },
    /// Regression test for INJECTION-side state accumulation bugs.
    /// Different bug class from `self-test ptt-wedge`: this verb probes
    /// the injection path itself (plan builder + guard bracket counter)
    /// for state that should reset between successive `inject()` calls
    /// but doesn't - modifier stickiness, character-position leaks,
    /// backend-selection cache staleness, and unbalanced `arm_start` /
    /// `arm_end` pairs.
    ///
    /// Each iteration builds a fresh [`crate::injection::plan::build_plan`]
    /// for a per-iteration payload, compares its derived fields against a
    /// reference plan for determinism, then round-trips
    /// [`crate::hotkey::inject_guard::InjectionGuard::arm_start`] /
    /// `arm_end` and asserts the guard's `is_active` returns to false
    /// past the post-grace horizon. Runs no OS side effects on the
    /// default (dry-run) path - safe in CI.
    ///
    /// `--live` opts into the dangerous path: `execute_plan` really types
    /// into the active window. NEVER passed by the smoke script or CI.
    ///
    /// Feature-gated behind `rust-hotkeys` + `rust-injection` - a stock
    /// build exits with an actionable "rebuild with --features ..."
    /// message rather than hanging or reporting a false pass.
    InjectionIdempotency {
        /// Iterations to run. Each iteration uses a fresh guard so a
        /// failure in one does not cascade into the next; the loop stops
        /// at the first failure so the report unambiguously names the
        /// broken cycle. Default 10 - an order of magnitude higher than
        /// `ptt-wedge` so intermittent state leaks are more likely to
        /// surface.
        #[arg(long, default_value_t = 10)]
        iterations: usize,
        /// Emit a single JSON object with `kind`, `backend`, `live`,
        /// `iterations`, `all_passed`, and per-iteration `results` - the
        /// machine-readable contract callers should pin.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Override the auto-picked backend. Accepts the same names as
        /// `inject-text --backend` (auto / pynput / wtype / ydotool /
        /// xdotool / kwtype / dotool / enigo / type / paste). Useful for
        /// exercising a specific injection path without waiting for the
        /// host platform's default to line up.
        #[arg(
            long,
            default_value = "auto",
            value_parser = [
                "auto", "pynput", "wtype", "ydotool", "xdotool", "kwtype",
                "dotool", "enigo", "type", "paste",
            ],
        )]
        backend: String,
        /// REALLY execute the plan (dangerous - types into the active
        /// window using the resolved backend). Off by default; the smoke
        /// script and CI never pass this. Use only on a scratch VM /
        /// unfocused window when hunting a live-only regression.
        ///
        /// Known limitation with `--live --backend paste`: the OS
        /// clipboard is shared across iterations. The harness inserts a
        /// spacer between iterations to let async clipboard writes
        /// flush, but stale content may still leak. Inspect each iteration's output
        /// manually rather than trusting the summary alone.
        #[arg(long, default_value_t = false)]
        live: bool,
    },
    /// Regression test for the audio-capture path (item 5 prereq 4 -
    /// PipeWire quantum handling and the Rust dictation path).
    /// Opens the cpal input stream for `--duration` seconds, tallies
    /// samples, and reports RMS + peak. Applies `PIPEWIRE_QUANTUM=2048`
    /// when unset on Linux
    /// BEFORE opening the stream so the fix is exercised on every run.
    ///
    /// Two failure modes this catches:
    ///   1. Stream opens but never delivers samples - `frames_captured == 0`
    ///      produces a non-zero exit.
    ///   2. Optionally, capture succeeds but the signal is silence
    ///      (permission / mute bug). Off by default; enable with
    ///      `--fail-on-silence` where a live device is guaranteed.
    ///
    /// Feature-gated behind `audio-capture` - a stock build exits with an
    /// actionable "rebuild with --features audio-capture" message rather
    /// than hanging or reporting a false pass. Safe in headless CI: a
    /// missing device fails gracefully with a distinctive error message
    /// the smoke script can grep for.
    AudioCapture {
        /// Capture duration in milliseconds. Sub-100 ms values risk
        /// missing even the first cpal callback on high-latency devices,
        /// so a stderr warning fires below that. Milliseconds (not
        /// seconds with a decimal) so the enum stays `Eq`-derivable
        /// alongside the other subcommands.
        #[arg(long, default_value_t = 2000)]
        duration_ms: u64,
        /// Input device selector. Same lookup precedence as `devices`
        /// (exact -> substring -> numeric); empty string picks the
        /// system default.
        #[arg(long, default_value = "")]
        device: String,
        /// Emit a single JSON object with the machine-readable contract
        /// (`kind`, `device`, `frames_captured`, `rms`, `peak`,
        /// `pipewire_quantum_branch`, ...).
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Promote "captured only silence" (RMS < 1e-6) from a warning
        /// to a hard failure. Off by default because most CI containers
        /// have no audio device wired to an ALSA loopback; enable only
        /// where a live audio source is guaranteed.
        #[arg(long, default_value_t = false)]
        fail_on_silence: bool,
    },
    /// Regression test for Whisper cold-load latency + OOM. Loads a GGML
    /// model through the diagnostic background preloader, reports wall-clock
    /// elapsed + on-disk file size, and exits non-zero on any load failure.
    ///
    /// Feature-gated behind `whisper-rs-local` - a stock build exits with
    /// an actionable "rebuild with --features whisper-rs-local" message
    /// rather than pretending to run the check.
    ///
    /// The verb DOES NOT download the model - it expects the file to
    /// already be in the cache (run `wd models download
    /// <name>` first). This keeps the self-test hermetic: a slow CI
    /// runner isn't punished by a HuggingFace fetch happening inside
    /// the elapsed measurement.
    WhisperLoad {
        /// Catalog model name (`tiny.en`, `base`, `small`, `medium`,
        /// `large-v3-turbo`, `large-v3`, or the multilingual siblings)
        /// or an absolute path to a custom GGML file. The catalog names
        /// resolve via [`crate::whisper::model_manager::find`]; anything
        /// containing `/` or `\`, or ending in `.bin` / `.gguf`, is
        /// treated as a literal path.
        ///
        /// Empty (the default) resolves to whichever tiny fixture is CACHED -
        /// see `whisper::self_test::default_tiny_fixture`. A hardcoded default
        /// fails before the loader runs on a box prepared with the other
        /// variant, which would silently skip the very regression this
        /// self-test exists to catch.
        #[arg(long, default_value = "")]
        model: String,
        /// Emit the [`crate::whisper::self_test::WhisperLoadReport`] as
        /// one JSON object with a stable field shape (`kind`, `model`,
        /// `path`, `file_size_bytes`, `elapsed_ms`, `status`, `ok`,
        /// `error`, `error_kind`). Consumed by
        /// `scripts/integration/wayland-user-smoke.sh` and the CI
        /// `integration-ubuntu-2604` job.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Feedback-cues self-test. Plays each enabled start, stop, or done cue
    /// through the same [`crate::dictate::feedback::SystemCueSink`] the live
    /// session uses, with `--delay-ms` between selected cues.
    /// Reports which backend fired (`kernel32_beep` / `paplay` / `pw-play`
    /// / `noop`). Exits non-zero when the env gate is on but no backend
    /// is available (the silent-mute regression this verb catches).
    Feedback {
        /// Sleep between selected cues in milliseconds. `0` still exercises
        /// the code path (playback happens on a detached thread).
        #[arg(long, default_value_t = 100)]
        delay_ms: u64,
        /// Emit a single JSON object with `kind`, `ok`, `error`,
        /// `env_enabled`, `backend`, `start_played`, `stop_played`, `done_played`,
        /// `delay_ms`.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Round 2/3 audio-ducker self-test. Enters the
    /// [`crate::dictate::audio_ducking::SystemAudioDucker`], sleeps for
    /// `--duration-ms`, then exits. Reports the resolved backend
    /// (`wasapi` / `unsupported_platform` / `feature_disabled`), the
    /// effective target volume, and whether enter+exit fired.
    AudioDucking {
        /// How long to hold the ducked state in milliseconds. Defaults
        /// to 500 ms - enough for a listener to hear media dampen.
        #[arg(long, default_value_t = 500)]
        duration_ms: u64,
        /// Emit a single JSON object with `kind`, `ok`, `backend`,
        /// `env_enabled`, `target_volume`, `duration_ms`, `entered`,
        /// `exited`, `error`.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Round 2/3 target-profile matcher self-test. Constructs a synthetic
    /// [`crate::platform::foreground_window::WindowInfo`] from
    /// `--title` / `--process` and runs the user's configured profile
    /// list against it via the same
    /// [`crate::dictate::profile::ReloadingProfileMatcher`] the live
    /// session uses.
    ProfileMatch {
        /// Window title to match. Empty (default) sends `None` to the
        /// matcher, mirroring a Wayland / macOS foreground probe.
        #[arg(long, default_value = "")]
        title: String,
        /// Process image name. Empty (default) sends `None`.
        #[arg(long, default_value = "")]
        process: String,
        /// Emit a single JSON object with `kind`, `ok`, `title`,
        /// `process`, `matched`, `profile`.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Round 2/3 history-sink self-test. Builds a synthetic utterance
    /// event carrying `--text`, runs it through
    /// [`crate::dictate::session::history_sink::history_sink_from_settings`]
    /// (which reads the same `VOICEPI_HISTORY_JSONL` /
    /// `VOICEPI_HISTORY_ENABLED` overlay the shipping session honours),
    /// and reports the resolved file path plus the filtered row.
    HistoryWrite {
        /// Utterance text to write. Empty string still writes a row
        /// (useful for pinning the shape).
        #[arg(long, default_value = "")]
        text: String,
        /// Emit a single JSON object with `kind`, `ok`, `enabled`,
        /// `path`, `row`, `bytes_written`, `error`.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Round 2/3 metrics-sink self-test. Same shape as `history-write`
    /// but for the metrics sink
    /// ([`crate::dictate::session::metrics_sink::metrics_sink_from_settings`]).
    /// Honours the two-part `VOICEPI_JSON` + `VOICEPI_METRICS_JSONL`
    /// gate. Reports `enabled=false, path=null` when either half of the
    /// gate is off (the correct "user did not opt in" answer).
    MetricsWrite {
        /// Utterance text to write.
        #[arg(long, default_value = "")]
        text: String,
        /// Emit a single JSON object with `kind`, `ok`, `enabled`,
        /// `path`, `row`, `bytes_written`, `error`.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Round 2/3 live-preview self-test. Spawns a real
    /// [`crate::dictate::PreviewEngine`] with a canned mock backend,
    /// pushes `--frames` fake PCM frames, waits for the tick, and
    /// collects the emissions. The pass signal is at least one emission
    /// for a non-empty canned text.
    Preview {
        /// Number of frames to push. Default `5`.
        #[arg(long, default_value_t = 5)]
        frames: usize,
        /// Samples per frame. Default `24000` (1.5 s @ 16 kHz), enough
        /// to clear the fresh-audio gate on the first tick.
        #[arg(long, default_value_t = 24_000)]
        frame_samples: usize,
        /// Sample rate the fake frames are at.
        #[arg(long, default_value_t = 16_000)]
        sample_rate: u32,
        /// Interval between preview ticks in milliseconds.
        #[arg(long, default_value_t = 100)]
        interval_ms: u64,
        /// Text the mock backend returns per tick. Empty string exercises
        /// the "backend returned nothing" branch (engine skips the
        /// emission - which is the correct behaviour).
        #[arg(long, default_value = "canned preview")]
        canned_text: String,
        /// Emit a single JSON object with `kind`, `ok`, `frames_pushed`,
        /// `frame_samples`, `sample_rate`, `interval_ms`, `emissions`.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Windows PTT-boot regression: installs the Rust hotkey subsystem
    /// (rdev / evdev) with the CURRENTLY-CONFIGURED PTT chord, waits
    /// `--for` seconds for the listener to prove it is alive, then
    /// unhooks and reports pass/fail. Catches the Windows GUI
    /// regression where `wd-gui.exe` starts but no PTT
    /// event ever fires - running this verb from PowerShell exercises
    /// the SAME `install_hotkey` path the Phase-B supervisor uses and
    /// surfaces `rdev` listener-startup errors that the GUI would
    /// discard because `windows_subsystem = "windows"` detaches from
    /// the parent console.
    ///
    /// This does NOT open the audio pump or load the Whisper model -
    /// the goal is to verify the OS hook, driver selection, and
    /// coordinator wiring only, so a run stays fast (`--for 2`) and
    /// does not need a mic / model on disk. For the full boot check
    /// including audio + model, use `dictate-run` directly.
    ///
    /// Feature-gated behind `rust-hotkeys` + `rust-injection` (the
    /// same combined features Phase A / Phase B of item 5 require).
    /// Stock builds exit with an actionable "rebuild with --features"
    /// message rather than reporting a false pass.
    HotkeyBoot {
        /// Milliseconds to hold the listener alive after install. The
        /// verb reports whether the listener stayed up for the full
        /// window - a Windows regression that lets rdev's `listen`
        /// return Ok prematurely (per-thread hook dies with the
        /// thread) would surface here as `listener_exited_early:
        /// true`. Default 2000 ms is enough to catch the fast-fail
        /// cases without slowing smoke scripts.
        ///
        /// Milliseconds (not seconds with a decimal) so the enum
        /// stays `Eq`-derivable alongside the other subcommands -
        /// matches the `AudioCapture` / `AudioDucking` convention.
        #[arg(long, default_value_t = 2000)]
        hold_ms: u64,
        /// Override the configured PTT chord. Accepts the same
        /// `+`-separated key names as the `key` setting (`ctrl_l`,
        /// `ctrl_l+shift_l`, `f9`, ...). Empty (the default) reads
        /// the current config so a user reproducing a bug hits the
        /// exact same install as the GUI.
        #[arg(long, default_value = "")]
        chord: String,
        /// Emit a single JSON object with `kind`, `ok`, `driver`,
        /// `chord`, `install_ms`, `listener_exited_early`,
        /// `install_error`. Machine-readable contract for CI /
        /// support-script pinning.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Force a specific hotkey driver for this self-test run. Same
        /// canonical names as the `hotkey capture --driver` flag
        /// (`auto`, `rdev`, `evdev`, `register`, plus the `x11` /
        /// `wayland` / `win_registerhotkey` / `wm_hotkey` aliases).
        /// `register` is the Windows `RegisterHotKey` backend the GUI
        /// binary uses to bypass the WH_KEYBOARD_LL hook chain - pass
        /// it here to reproduce a GUI-side wedge fix from the CLI
        /// where stderr is visible.
        ///
        /// The flag sets `VOICEPI_HOTKEY_DRIVER` for this process
        /// only, so the same selection logic the shipping install
        /// consults fires here. Unrecognised values are rejected up-
        /// front (matches the `hotkey capture` fail-fast policy).
        #[arg(long, value_name = "AUTO|RDEV|EVDEV|REGISTER", default_value = "auto")]
        driver: String,
    },
}
