//! Desktop UI module root. This file is deliberately thin: it wires up the UI
//! submodules, re-exports their shared items into `crate::ui` so the settings
//! tabs (which import `super::super::*`) keep resolving, and owns the small
//! cross-cutting definitions — the model option tables, the `WhisperDictateApp`
//! state struct, its `Default`, the `Tab` enum, and the `eframe` `run` entry.
//!
//! Behaviour lives in the submodules:
//! - `app`            — the `eframe::App` `update` loop + runtime lifecycle/polling
//! - `tasks`          — background doctor/install runs and API connectivity checks
//! - `settings_state` — config save/reload + provider/API-key persistence
//! - `previews`       — dictionary/history/metrics file helpers and previews
//! - `theme`          — palette, colour/dimension constants, egui style + chrome
//! - `text`           — localized UI strings (`UiTextKey`/`ui_text`)
//! - `widgets`        — reusable settings-grid form rows and help badges
//! - `log_render`     — runtime-log view modes and card parsing
//! - `worker_event`   — worker JSON event parsing + audio-meter helpers
//! - `platform`       — STT backend mode, XKB layout detection, `open_url`
//! - `api_keys`       — secret storage / cloud + post provider model
//! - `tabs`           — per-tab rendering (`impl WhisperDictateApp` UI methods)

use anyhow::Result;
use eframe::egui;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::config::{self, AppSettings};
use crate::runtime::{self, RuntimeState, RuntimeSupervisor};
// Re-exported only for the headless `*_tests.rs` modules that build worker events
// via `super::*`; non-test code imports `WorkerEvent` from `crate::runtime`.
#[cfg(test)]
pub(in crate::ui) use crate::runtime::WorkerEvent;

mod api_keys;
mod app;
mod audio_devices;
mod benchmark_results;
mod corpus;
mod corpus_batch;
mod corpus_record;
mod corpus_record_tasks;
mod device_test;
mod diagnostics_level;
mod hotkey;
mod icon;
mod log_render;
mod onboarding;
mod overlay;
mod platform;
mod previews;
mod secret_store;
mod settings_state;
mod tabs;
mod tasks;
mod text;
mod text_scale;
mod theme;
mod tray;
mod update_check;
mod upgrade_hint;
mod whisper_models_state;
mod widgets;
mod widgets_combo;
mod window_list;
mod worker_event;
mod worker_json;

use self::api_keys::*;
pub(in crate::ui) use self::audio_devices::parse_audio_devices_json;
pub(in crate::ui) use self::benchmark_results::*;
pub(in crate::ui) use self::corpus::*;
pub(in crate::ui) use self::corpus_batch::*;
pub(in crate::ui) use self::corpus_record::*;
pub(in crate::ui) use self::corpus_record_tasks::*;
pub(in crate::ui) use self::device_test::*;
pub(in crate::ui) use self::diagnostics_level::*;
pub(in crate::ui) use self::hotkey::*;
use self::icon::app_icon;
pub(in crate::ui) use self::window_list::parse_windows_json;
// Re-exported so the secret-store `*_tests.rs` modules (which import `super::*`)
// resolve these items; non-test code reaches them through `api_keys`.
pub(in crate::ui) use self::log_render::*;
pub(in crate::ui) use self::overlay::{
    render_recording_overlay, MeterFrame, OverlayConfig, OverlayPalette, OverlayPhase,
    OverlayPosition, OverlayRender, OverlayState,
};
pub(in crate::ui) use self::platform::*;
#[cfg(test)]
use self::secret_store::*;
pub(in crate::ui) use self::text::*;
pub(in crate::ui) use self::text_scale::*;
pub(in crate::ui) use self::theme::*;
pub(in crate::ui) use self::tray::*;
pub(in crate::ui) use self::update_check::*;
pub(in crate::ui) use self::upgrade_hint::*;
pub(in crate::ui) use self::widgets::*;
pub(in crate::ui) use self::worker_event::*;

// Ordered most → least accurate. Larger models are more accurate but slower
// and need more VRAM; see `whisper_model_hint` for per-model annotations.
const WHISPER_MODELS: &[&str] = &[
    "large-v3",
    "large-v3-turbo",
    "medium",
    "small",
    "base",
    "tiny",
];

/// Accuracy/speed note + approximate VRAM (MB, at the GPU `int8_float16`
/// default) for a Whisper model value. Drives the model picker's labels and
/// the "does it fit my GPU" grey-out.
fn whisper_model_hint(model: &str) -> (&'static str, u32) {
    match model {
        "large-v3" => ("most accurate, slowest", 3200),
        "large-v3-turbo" => ("great accuracy, fast", 1800),
        "medium" => ("good accuracy, lighter", 1500),
        "small" => ("ok accuracy, fast & light", 1000),
        "base" => ("low accuracy, very light", 600),
        "tiny" => ("lowest accuracy, fastest", 400),
        _ => ("", 0),
    }
}

/// Spawn a background thread that queries NVIDIA GPUs and sends the best total
/// VRAM (or `None`) over the returned channel.  This keeps GPU detection off
/// the startup / first-frame path so a missing nvidia-smi doesn't stall the UI.
fn spawn_gpu_probe() -> Receiver<Option<u32>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = crate::model_capacity::query_gpus()
            .iter()
            .map(|gpu| gpu.total_mb)
            .max();
        let _ = tx.send(result);
    });
    rx
}

/// Spawn a background thread that fetches the public version feed and computes
/// whether a strictly-newer version exists, sending an [`UpdateCheckOutcome`]
/// over the returned channel exactly once. Mirrors `spawn_gpu_probe`'s one-shot
/// channel discipline but is dispatched periodically by the app `update()` loop.
///
/// Three outcomes are distinguished so the caller can react correctly:
/// - `Newer(v)` — feed reachable, newer version found.
/// - `UpToDate`  — feed reachable, no newer version.
/// - `Failed`    — fetch / parse error; caller must NOT clear a prior badge.
fn spawn_update_check(
    current_version: String,
    include_prereleases: bool,
) -> Receiver<UpdateCheckOutcome> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = match update_check::fetch_published_versions() {
            Ok(versions) => {
                match update_check::latest_newer_version(
                    &versions,
                    &current_version,
                    include_prereleases,
                ) {
                    Some(v) => UpdateCheckOutcome::Newer(v),
                    None => UpdateCheckOutcome::UpToDate,
                }
            }
            Err(_) => UpdateCheckOutcome::Failed,
        };
        let _ = tx.send(outcome);
    });
    rx
}

const GROQ_STT_MODELS: &[&str] = &[
    "whisper-large-v3-turbo",
    "whisper-large-v3",
    "distil-whisper-large-v3-en",
];
const OPENAI_STT_MODELS: &[&str] = &["gpt-4o-mini-transcribe", "gpt-4o-transcribe", "whisper-1"];
// Wave 8 of #348 removed the `"parakeet"` entry here together with the
// NeMo backend. A saved `stt_backend = "parakeet"` is migrated to whisper
// at config-load time (see crate::config::load::migrate_parakeet_backend),
// so the picker stays a closed set of the values the worker actually
// accepts.
const STT_BACKEND_OPTIONS: &[(&str, &str)] = &[
    ("whisper", "Local Whisper"),
    ("openai", "Cloud STT (Groq/OpenAI)"),
];
const CLOUD_PROVIDER_OPTIONS: &[(&str, &str)] = &[
    ("groq", "Groq"),
    ("openai", "OpenAI"),
    ("custom", "Custom (OpenAI-compatible)"),
];
const POST_PROCESSOR_OPTIONS: &[(&str, &str)] = &[
    ("none", "Disabled"),
    ("ollama", "Local Ollama"),
    ("openai", "OpenAI"),
    ("groq", "Groq"),
];
const GROQ_POST_MODELS: &[(&str, &str)] = &[
    (
        "llama-3.3-70b-versatile",
        "llama-3.3-70b-versatile - recommended Danish final check",
    ),
    (
        "qwen/qwen3-32b",
        "qwen/qwen3-32b - strong multilingual, use hidden reasoning",
    ),
    (
        "openai/gpt-oss-20b",
        "openai/gpt-oss-20b - fast quality/cost candidate",
    ),
    (
        "openai/gpt-oss-120b",
        "openai/gpt-oss-120b - highest quality, heavier",
    ),
    (
        "llama-3.1-8b-instant",
        "llama-3.1-8b-instant - fastest simple cleanup",
    ),
    (
        "meta-llama/llama-4-scout-17b-16e-instruct",
        "llama-4-scout-17b - preview, not preferred for Danish",
    ),
    (
        "groq/compound-mini",
        "groq/compound-mini - agentic, not cleanup default",
    ),
    (
        "groq/compound",
        "groq/compound - agentic, not cleanup default",
    ),
];
const OPENAI_POST_MODELS: &[&str] = &["gpt-4o-mini", "gpt-4o", "gpt-4.1-mini"];

pub fn run() -> Result<()> {
    // Pick the renderer by Cargo feature. We ship on glow (the `default`
    // feature → `ui-egui-glow`); wgpu is the continuously-validated exit route,
    // opted into with `--no-default-features --features ui-egui-wgpu`. The two
    // renderer features are NOT strictly mutually exclusive: the cfg below makes
    // wgpu WIN whenever `ui-egui-wgpu` is enabled (even if `ui-egui-glow` is also
    // on), and falls back to glow otherwise — so a plain `cargo build` (default
    // features) is always glow. See [features] in Cargo.toml and renderer-matrix CI.
    #[cfg(feature = "ui-egui-wgpu")]
    let renderer = eframe::Renderer::Wgpu;
    #[cfg(not(feature = "ui-egui-wgpu"))]
    let renderer = eframe::Renderer::Glow;

    // Issue #326: write the daemon PID file and (on Linux) install the
    // SIGUSR1/SIGUSR2 handlers BEFORE eframe takes the main thread. The
    // returned guard removes the PID file on a clean exit; we bind it to a
    // variable that lives for the whole UI lifetime so its Drop fires at
    // shutdown. A failure here is logged but does NOT abort the UI — the
    // user still gets a working app, just without external-toggle support
    // for this session.
    let _external_toggle_guard = match runtime::external_toggle::install_signal_handlers() {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!(
                "[external-toggle] could not install signal handlers / PID file: {err}; \
                 `whisper-dictate --toggle-recording` and `kill -USR1` will not work \
                 for this session"
            );
            None
        }
    };

    let options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 760.0])
            // Floor the window so the top status bar can't be squeezed until the
            // Start/Stop controls overlap the Status/Backend/Model cards. Below
            // this width there isn't room for the sidebar + all status cards +
            // the runtime controls on one row.
            .with_min_inner_size([1000.0, 640.0])
            .with_app_id("whisper-dictate")
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        &format!("whisper-dictate {}", runtime::version()),
        options,
        Box::new(|cc| {
            egui_material_icons::initialize(&cc.egui_ctx);
            Ok(Box::new(WhisperDictateApp::default()))
        }),
    )
    .map_err(|err| anyhow::anyhow!(err.to_string()))
}

struct WhisperDictateApp {
    app_version: String,
    selected_tab: Tab,
    runtime_state: RuntimeState,
    runtime_log: String,
    runtime_log_scroll_to_bottom: bool,
    runtime_log_view: LogViewMode,
    audio_capture_opening: bool,
    audio_capture_active: bool,
    audio_meter_level: f32,
    audio_meter_raw_dbfs: Option<f32>,
    audio_meter_peak: Option<f32>,
    active_audio_device: String,
    /// Input devices offered by the Microphone picker, refreshed on demand via
    /// the worker's `--list-audio-devices`. Holds the raw device names that map
    /// to the persisted `settings.audio_device`; the combo always offers
    /// "(System default)" → "" ahead of these.
    audio_device_options: Vec<String>,
    /// One-shot guard for the automatic microphone-list load. Starts `false` and
    /// flips to `true` the first time the worker's `--list-audio-devices` run is
    /// dispatched at startup, so the picker is populated without the user having
    /// to click "Refresh devices". The manual button still re-scans on demand.
    audio_devices_loaded: bool,
    /// Visible top-level windows refreshed on demand via `--list-windows`.
    /// Each entry is `(title, process)`. Shown in the Profiles tab so the user
    /// can pick a window and insert a matching profile object.
    window_options: Vec<(String, String)>,
    /// Transient (non-persisted) banner shown when the worker reports the
    /// selected microphone is unusable (a `status` event with
    /// `state="error"`/`reason="device_unusable"`). Holds the actionable message
    /// to show prominently near the live-dictation header. Cleared as soon as a
    /// subsequent status/audio event reports a working `audio_device`, on
    /// start/stop/restart, and on worker exit/error.
    device_error: Option<String>,
    /// Transient (non-persisted) result of the Microphone "Test" button: the
    /// parsed ✓/⚠/✗ display model on success, or an `Err` message when the test
    /// run/parse failed. `None` before any test is run. Cleared when a new test
    /// is dispatched or the saved microphone changes.
    device_test_result: Option<Result<DeviceTestDisplay, String>>,
    /// Golden-benchmark corpus items offered by the System tab's "Record corpus
    /// audio" picker, loaded once from `benchmark/corpus.json` (app-root, falling
    /// back to appdata). Empty when no corpus is found — the picker then shows a
    /// "no items" placeholder.
    corpus_items: Vec<CorpusItem>,
    /// One-shot guard for the lazy corpus load: flips to `true` the first time
    /// the System tab is shown and the corpus is read from disk, so re-rendering
    /// the tab doesn't re-read the file every frame.
    corpus_loaded: bool,
    /// The corpus item id currently selected in the picker, or `None` before any
    /// selection. Passed to `--record-corpus-item` when Record is clicked.
    corpus_selected_id: Option<String>,
    /// Cached set of corpus item IDs that already have a recording under appdata.
    /// Populated by [`recorded_ids_set`] in `ensure_corpus_loaded` (and refreshed
    /// after every successful recording) so [`combo_entry_label`] can check
    /// recording presence in O(1) with no per-frame filesystem I/O.
    corpus_recorded_ids: std::collections::HashSet<String>,
    /// Transient (non-persisted) result of the last "Record" run: the parsed
    /// saved/failed outcome on success, or an `Err` message when the run/parse
    /// failed. `None` before any recording. Cleared when a new recording starts.
    corpus_record_result: Option<Result<CorpusRecordOutcome, String>>,
    /// Transient (non-persisted) batch recording cursor. `Some` while a "Record
    /// all"/"Record all missing" sequence is walking the corpus item-by-item;
    /// each item still uses the proven single-item `--record-corpus-item` worker,
    /// chained in the UI on every done-event. `None` outside a batch run. Dropped
    /// on completion or when the user clicks Stop. Never persisted.
    corpus_batch: Option<CorpusBatch>,
    /// Transient (non-persisted) deadline for the small breathing gap between
    /// batch clips: set to `Instant::now() + gap` when a clip finishes, and the
    /// next clip is launched once `Instant::now()` passes it (checked each frame
    /// in `poll_corpus_batch`). `None` when no launch is pending.
    corpus_batch_resume_at: Option<Instant>,
    /// Transient (non-persisted) parsed results of the last "Run benchmark" run:
    /// the per-item rows + aggregate summary the System tab renders as a
    /// digestible headline + table (instead of the raw JSONL wall). `None` before
    /// any run. Set when the run-benchmark background task COMPLETES (parsed from
    /// the captured stdout) and cleared when a new run starts.
    benchmark_results: Option<BenchmarkResults>,
    config_path: String,
    settings: AppSettings,
    saved_settings: AppSettings,
    settings_status: String,
    stt_api_key_input: String,
    saved_stt_api_key_input: String,
    stt_api_key_reveal_until: Option<Instant>,
    stt_api_key_status: String,
    post_api_key_input: String,
    saved_post_api_key_input: String,
    post_api_key_reveal_until: Option<Instant>,
    post_api_key_status: String,
    dictionary_preview: String,
    history_preview: String,
    metrics_preview: String,
    /// One-shot flags: set when a preview is (re)loaded so its next render
    /// scrolls the freshly rendered preview into view (it would otherwise land
    /// below the settings ScrollArea fold and read as "the button did nothing").
    /// In egui's immediate mode this usually happens within the same UI pass as
    /// the click. Cleared after the scroll. Mirrors `runtime_log_scroll_to_bottom`.
    scroll_to_history_preview: bool,
    scroll_to_metrics_preview: bool,
    supervisor: RuntimeSupervisor,
    background_task: Option<Receiver<BackgroundTaskResult>>,
    background_task_label: Option<&'static str>,
    /// Best total VRAM (MB) of the detected NVIDIA GPU, or None on CPU /
    /// non-NVIDIA. Populated asynchronously after startup via `gpu_probe`; gates
    /// the Whisper model picker (no grey-out while still probing).
    gpu_total_mb: Option<u32>,
    /// Background thread that computes `gpu_total_mb` (runs `nvidia-smi`).
    /// Polled non-blockingly each frame; set to `None` once the result is adopted.
    gpu_probe: Option<Receiver<Option<u32>>>,
    /// The raw worker `status` state string from the most recent status event
    /// (e.g. `"opening"`, `"recording"`, `"transcribing"`, `"ready"`, …).
    /// Stored verbatim so `sync_tray` can pass it to `tray_state_for`, which
    /// correctly maps `"opening"` → amber (mic not yet live) before `"recording"`
    /// → red (mic live). Empty string before the first status event arrives,
    /// which `tray_state_for` treats as the `Ready` fallback when the worker is
    /// running — an acceptable approximation for the very first frame.
    last_worker_status_state: String,
    /// Current dictation pipeline stage from worker status events
    /// ("recording" / "transcribing" / "post-processing"), or None when idle.
    /// Drives the live progress card in the runtime log.
    pipeline_stage: Option<&'static str>,
    /// Live partial-transcription preview text emitted while recording
    /// (worker `state="preview"`). Shown as a muted second line in the recording
    /// card so the user watches the sentence grow; cleared when the utterance
    /// settles. Display-only — the final injected text comes from the utterance
    /// event, never from this.
    pipeline_preview: Option<String>,
    /// Whether the worker has finished loading the model and is ready to receive
    /// speech. The OS process spawns near-instantly (RuntimeState::Running), but
    /// loading a local model takes time, so the status stays "Starting" until the
    /// worker reports `ready`. Reset on each start/restart and on exit/error.
    worker_ready: bool,
    /// Instant at which the most recent `start_runtime` succeeded. Used to
    /// detect fast crashes (exit within 10 s of start).
    worker_start_time: Option<Instant>,
    /// Number of consecutive fast non-zero exits since the last clean run.
    /// Reset when the worker reports ready, exits cleanly, or runs ≥ 10 s.
    fast_crash_count: u32,
    /// Session-only UI flag: when set, `update()` renders the minimal always-on-top
    /// compact strip (Start/Stop + mic) instead of the full window. NOT persisted
    /// to config — it only resizes/levels the existing viewport; the worker keeps
    /// running across the switch. Toggled from the top status bar and reset to
    /// `false` on launch.
    compact_mode: bool,
    /// The newest published version when it is strictly newer than the running
    /// one, driving the discreet sidebar "update available" badge. `None` when up
    /// to date, when the check is disabled/local-only, or before the first poll.
    /// Populated only from the background poll's channel — never blocks the UI.
    update_available: Option<String>,
    /// Monotonic timestamp of the last completed/started update poll. Drives the
    /// "elapsed >= interval" gate. `Instant` (not wall-clock) so clock changes /
    /// sleep can't skew the cadence. `None` until the first poll is dispatched.
    last_update_check: Option<Instant>,
    /// Receiver for the in-flight background update check. `Some` while a single
    /// poll thread is running (the one-shot-per-cycle guard), `None` otherwise.
    /// The thread sends an [`UpdateCheckOutcome`] exactly once.
    update_check_rx: Option<Receiver<UpdateCheckOutcome>>,
    /// Instant until which the transient "Copied!" confirmation is shown next to
    /// the update badge after the upgrade command is copied to the clipboard.
    /// Session-only UI state — `None` when no confirmation is pending.
    update_command_copied_until: Option<Instant>,
    /// Cached result of the one-time check for the Chocolatey package directory
    /// (`%ProgramData%\chocolatey\lib\whisper-dictate` or via `$ChocolateyInstall`).
    /// `None` until the first update badge render runs the probe lazily; the
    /// filesystem is then never re-probed. On non-Windows the probe is a stub
    /// that resolves to `false` without touching the filesystem (Chocolatey is
    /// Windows-only).
    choco_pkg_dir_exists: Option<bool>,
    /// System-tray (notification-area) icon manager. Created empty; the actual
    /// OS tray is built lazily on the first `update()` frame (Windows only — a
    /// no-op stub elsewhere) and recolours to mirror the dictation state so the
    /// user can SEE when the microphone is live. Purely additive UI — it never
    /// touches the dictation flow.
    tray: TrayManager,
    /// Last [`TrayState`] we wrote a diagnostic log line for, so the "tray sync"
    /// trace only fires on TRANSITIONS — not on every frame. `None` means we
    /// have not yet logged any state (so the very first computed state is
    /// always logged as the baseline). See `sync_tray` in `ui/app.rs`.
    pub(in crate::ui) last_logged_tray_state: Option<TrayState>,
    /// Live download state for the Settings tab's "Whisper model" section
    /// (Wave 7-B). Shared via `Arc<Mutex<…>>` with each download worker
    /// thread so progress updates land in the same map the UI polls.
    /// Empty when no downloads have been kicked off this session — never
    /// persisted.
    pub(in crate::ui) whisper_model_downloads: whisper_models_state::WhisperModelDownloads,
    /// Time-smoothed audio meter + visibility bookkeeping for the recording
    /// overlay window (Issue #320). Lives outside `AppSettings` because it is
    /// session-only render state — the persisted toggle/position live in the
    /// settings struct (`overlay_enabled` / `overlay_position` /
    /// `overlay_show_on_idle`).
    pub(in crate::ui) overlay_state: OverlayState,
    /// Issue #328: first-run onboarding wizard state. `Some` when the wizard
    /// is on screen (fresh install or "Run setup again" from the System
    /// tab); `None` otherwise. Session-only — the persisted gate lives on
    /// `AppSettings.onboarding_completed`.
    pub(in crate::ui) onboarding: Option<onboarding::OnboardingUi>,
}

impl Default for WhisperDictateApp {
    fn default() -> Self {
        let (mut settings, settings_status) = match config::load_settings() {
            Ok(settings) => (settings, String::new()),
            Err(err) => (
                AppSettings::default(),
                format!("Could not load config, using defaults: {err}"),
            ),
        };
        let provider = CloudProvider::from_settings(&settings);
        let (stt_api_key_input, saved_stt_api_key_input, stt_api_key_status) =
            load_stt_api_key_state(provider).unwrap_or_else(|err| {
                (
                    String::new(),
                    String::new(),
                    format!("Could not load API key: {err}"),
                )
            });
        let (post_api_key_input, saved_post_api_key_input, post_api_key_status) =
            load_post_api_key_state(PostProvider::from_settings(&settings)).unwrap_or_else(|err| {
                (
                    String::new(),
                    String::new(),
                    format!("Could not load post-processing API key: {err}"),
                )
            });
        let config_path = config::config_path().display().to_string();
        // Prefill the Metrics JSONL field with the default path next to config.json
        // when it is empty, so the field shows a real, copyable location. This is
        // applied to BOTH `settings` and the `saved_settings` baseline below, so it
        // never flags the form as having unsaved changes. Metrics are still only
        // written while "JSON stdout" is enabled, so a prefilled path is harmless.
        if settings.metrics_jsonl.trim().is_empty() {
            settings.metrics_jsonl = tabs::default_metrics_jsonl_path(&config_path);
        }
        let runtime_log = format!(
            "Rust UI ready. Start launches the Python dictation worker directly.\n[ui] config: {config_path}\n[ui] cloud API key load: {stt_api_key_status}\n[ui] post API key load: {post_api_key_status}"
        );
        Self {
            app_version: runtime::version(),
            selected_tab: Tab::Log,
            runtime_state: RuntimeState::Stopped,
            runtime_log,
            runtime_log_scroll_to_bottom: true,
            runtime_log_view: LogViewMode::from_raw(&settings.ui_log_view),
            audio_capture_opening: false,
            audio_capture_active: false,
            audio_meter_level: 0.0,
            audio_meter_raw_dbfs: None,
            audio_meter_peak: None,
            active_audio_device: String::new(),
            audio_device_options: Vec::new(),
            audio_devices_loaded: false,
            window_options: Vec::new(),
            device_error: None,
            device_test_result: None,
            corpus_items: Vec::new(),
            corpus_loaded: false,
            corpus_selected_id: None,
            corpus_recorded_ids: std::collections::HashSet::new(),
            corpus_record_result: None,
            corpus_batch: None,
            corpus_batch_resume_at: None,
            benchmark_results: None,
            config_path,
            saved_settings: settings.clone(),
            // Trigger the wizard on first launch (settings default is
            // `onboarding_completed = false`); users flip that on skip/finish.
            // On a bare skip (no "don't show again") the gate stays false and
            // the wizard re-triggers on next launch. See
            // `onboarding::should_trigger_first_run`. Computed BEFORE `settings`
            // is moved into the struct.
            onboarding: onboarding::should_trigger_first_run(&settings)
                .then(onboarding::OnboardingUi::new),
            settings,
            settings_status,
            saved_stt_api_key_input,
            stt_api_key_input,
            stt_api_key_reveal_until: None,
            stt_api_key_status,
            saved_post_api_key_input,
            post_api_key_input,
            post_api_key_reveal_until: None,
            post_api_key_status,
            dictionary_preview: String::new(),
            history_preview: String::new(),
            metrics_preview: String::new(),
            scroll_to_history_preview: false,
            scroll_to_metrics_preview: false,
            supervisor: RuntimeSupervisor::new(),
            background_task: None,
            background_task_label: None,
            gpu_total_mb: None,
            gpu_probe: Some(spawn_gpu_probe()),
            last_worker_status_state: String::new(),
            pipeline_stage: None,
            pipeline_preview: None,
            worker_ready: false,
            worker_start_time: None,
            fast_crash_count: 0,
            compact_mode: false,
            update_available: None,
            last_update_check: None,
            update_check_rx: None,
            update_command_copied_until: None,
            choco_pkg_dir_exists: None,
            tray: TrayManager::new(),
            last_logged_tray_state: None,
            whisper_model_downloads: whisper_models_state::WhisperModelDownloads::new(),
            overlay_state: OverlayState::default(),
        }
    }
}

struct BackgroundTaskResult {
    label: &'static str,
    command: String,
    stdout: String,
    stderr: String,
    success: bool,
    code: Option<i32>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Tab {
    Log,
    Speech,
    Quality,
    Dictionary,
    Output,
    Post,
    Profiles,
    System,
}

impl Tab {
    pub(in crate::ui) const ALL: [Tab; 8] = [
        Tab::Log,
        Tab::Speech,
        Tab::Quality,
        Tab::Dictionary,
        Tab::Output,
        Tab::Post,
        Tab::Profiles,
        Tab::System,
    ];

    pub(in crate::ui) fn label(self, raw_language: &str) -> &'static str {
        match self {
            Tab::Log => ui_text(raw_language, UiTextKey::Log),
            Tab::Speech => ui_text(raw_language, UiTextKey::Speech),
            Tab::Quality => ui_text(raw_language, UiTextKey::Quality),
            Tab::Dictionary => ui_text(raw_language, UiTextKey::Dictionary),
            Tab::Output => ui_text(raw_language, UiTextKey::Output),
            Tab::Post => ui_text(raw_language, UiTextKey::Post),
            Tab::Profiles => ui_text(raw_language, UiTextKey::Profiles),
            Tab::System => ui_text(raw_language, UiTextKey::System),
        }
    }

    pub(in crate::ui) fn icon(self) -> &'static str {
        match self {
            Tab::Log => egui_material_icons::icons::ICON_ARTICLE.codepoint,
            Tab::Speech => egui_material_icons::icons::ICON_MIC.codepoint,
            Tab::Quality => egui_material_icons::icons::ICON_GRAPHIC_EQ.codepoint,
            Tab::Dictionary => egui_material_icons::icons::ICON_BOOK.codepoint,
            Tab::Output => egui_material_icons::icons::ICON_OUTPUT.codepoint,
            Tab::Post => egui_material_icons::icons::ICON_AUTO_FIX_HIGH.codepoint,
            Tab::Profiles => egui_material_icons::icons::ICON_GROUP.codepoint,
            Tab::System => egui_material_icons::icons::ICON_SETTINGS.codepoint,
        }
    }
}

#[cfg(test)]
mod api_key_env_tests;
#[cfg(test)]
mod api_key_store_tests;
#[cfg(test)]
mod audio_device_picker_tests;
#[cfg(test)]
mod backend_option_tests;
#[cfg(test)]
mod benchmark_task_tests;
#[cfg(test)]
mod cloud_settings_tests;
#[cfg(test)]
mod corpus_record_task_tests;
#[cfg(test)]
mod keyboard_layout_tests;
#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod log_view_tests;
#[cfg(test)]
mod model_picker_tests;
#[cfg(test)]
mod robustness_tests;
#[cfg(test)]
mod runtime_status_tests;
#[cfg(test)]
mod settings_reset_tests;
#[cfg(test)]
mod tab_helpers_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod ui_language_tests;
#[cfg(test)]
mod update_check_tests;
