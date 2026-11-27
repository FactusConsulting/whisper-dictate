use super::*;
use crate::ui::render_test_support::painted_texts_at;
use crate::ui::test_support::{test_app, EnvVarGuard};
use crate::ui::{ui_palette, AppSettings};

fn app() -> WhisperDictateApp {
    test_app(AppSettings {
        ui_language: "en".to_owned(),
        ui_autostart_runtime: false,
        ..Default::default()
    })
}

fn assert_readout(app: &mut WhisperDictateApp, expected: &str) {
    for (compact, size) in [(false, [1000.0, 640.0]), (true, [460.0, 210.0])] {
        app.compact_mode = compact;
        let painted = painted_texts_at(app, egui::vec2(size[0], size[1]));
        let label = painted
            .iter()
            .find(|label| label.text.contains(expected))
            .unwrap_or_else(|| panic!("missing {expected:?} with compact={compact}"));
        assert!(label.rect.left() >= 0.0 && label.rect.right() <= size[0]);
        assert!(label.rect.top() >= 0.0 && label.rect.bottom() <= size[1]);
    }
}

fn start_enabled(app: &WhisperDictateApp) -> bool {
    let ctx = egui::Context::default();
    let mut enabled = false;
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        enabled = app
            .manual_start_button(
                ui,
                ui_palette(&app.settings.ui_theme),
                egui::vec2(88.0, 34.0),
            )
            .enabled();
    });
    output.textures_delta.clear();
    enabled
}

fn measured(app: &mut WhisperDictateApp, check: LocalGpuCheck) {
    let (sender, receiver) = mpsc::channel();
    let now = Instant::now();
    sender.send((check, now)).unwrap();
    app.gpu_start.pending = Some(PendingSample {
        receiver,
        obsolete: false,
    });
    app.gpu_start.complete_sample(now);
}

fn finish_sample(app: &mut WhisperDictateApp, mut poll: impl FnMut(&mut WhisperDictateApp)) {
    assert!(app.gpu_start.pending.is_some(), "sample was not scheduled");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        poll(app);
        if app.gpu_start.pending.is_none() {
            return;
        }
        assert!(Instant::now() < deadline, "GPU sample did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn no_sample(_: &LocalGpuTarget) -> (LocalGpuCheck, Instant) {
    panic!("GPU sample was not due");
}

fn capacity(free_mib: u32) -> LocalGpuCheck {
    LocalGpuCheck::Capacity {
        required_mib: 4096,
        free_mib: Some(free_mib),
    }
}

#[test]
fn asynchronous_refresh_keeps_last_budget_until_recovery() {
    let mut app = app();
    let ctx = egui::Context::default();
    let now = Instant::now();
    app.gpu_start
        .set_selection(Selection::Target(LocalGpuTarget::Whisper(4096)));
    let (finish, result) = mpsc::channel();
    app.gpu_start.sample(&ctx, now, move |target| {
        assert_eq!(*target, LocalGpuTarget::Whisper(4096));
        (result.recv().unwrap(), now)
    });
    app.gpu_start.sample(&ctx, now, no_sample);
    assert_readout(&mut app, "Checking free GPU memory");
    assert!(!start_enabled(&app));

    finish.send(capacity(512)).unwrap();
    finish_sample(&mut app, |app| app.gpu_start.sample(&ctx, now, no_sample));
    assert_readout(&mut app, "512/4096 MiB");
    assert_readout(&mut app, "too low");
    assert!(!start_enabled(&app));

    let due = now + REFRESH_INTERVAL;
    app.gpu_start
        .sample(&ctx, due - Duration::from_millis(1), no_sample);
    assert!(app.gpu_start.pending.is_none());
    app.gpu_start
        .sample(&ctx, due, move |_| (capacity(4096), due));

    app.settings.ui_language = "da".to_owned();
    assert_readout(&mut app, "512/4096 MiB (ledig/krævet) · for lidt");
    assert!(!start_enabled(&app));
    finish_sample(&mut app, |app| app.gpu_start.sample(&ctx, due, no_sample));
    assert_readout(&mut app, "4096/4096 MiB (ledig/krævet) · klar");
    assert!(start_enabled(&app));
    app.settings.ui_language = "en".to_owned();
    assert_readout(&mut app, "4096/4096 MiB (free/needed) · ready");
}

#[test]
fn stale_budget_after_idle_blocks_start_until_resampled() {
    let mut app = app();
    let ctx = egui::Context::default();
    let now = Instant::now();
    app.gpu_start
        .set_selection(Selection::Target(LocalGpuTarget::Whisper(4096)));
    app.gpu_start
        .sample(&ctx, now, move |_| (capacity(8192), now));
    // A visible poll can consume the sample later than the worker measured it.
    let first_readout = now + REFRESH_INTERVAL / 2;
    finish_sample(&mut app, |app| {
        app.gpu_start.sample(&ctx, first_readout, no_sample);
    });
    assert!(start_enabled(&app));

    // No visible polls for two intervals: the old readout must not survive.
    let resumed = now + 2 * REFRESH_INTERVAL;
    app.gpu_start
        .sample(&ctx, resumed, move |_| (capacity(512), resumed));
    app.settings.ui_language = "da".to_owned();
    assert_readout(&mut app, "Kontrollerer ledig GPU-hukommelse");
    assert!(!start_enabled(&app));
    finish_sample(&mut app, |app| {
        app.gpu_start.sample(&ctx, resumed, no_sample);
    });
    assert_readout(&mut app, "512/4096 MiB");
    assert!(!start_enabled(&app));
}

#[test]
fn app_poll_suspends_while_running_and_discards_a_probe_after_saved_route_changes() {
    let _lock = crate::test_env_lock::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let missing = dir.path().join("missing.bin");
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());
    let _path = EnvVarGuard::set(
        crate::ui::app::WHISPER_MODEL_PATH_ENV,
        &missing.to_string_lossy(),
    );
    let _gpu = EnvVarGuard::set(crate::whisper::GPU_ENV, "auto");
    std::fs::write(&config, r#"{"stt_backend":"whisper","device":"auto"}"#).unwrap();

    let mut app = app();
    let ctx = egui::Context::default();
    assert!(
        effective_selection(&app.saved_worker_base_command().runtime)
            == Selection::Target(LocalGpuTarget::WhisperFile(missing))
    );
    app.gpu_start.manual = false;
    app.poll_gpu_start(&ctx);
    assert!(matches!(
        app.gpu_start_status(),
        Some(GpuStartStatus::Checking)
    ));
    assert!(!start_enabled(&app));
    finish_sample(&mut app, |app| app.poll_gpu_start(&ctx));
    assert_eq!(app.gpu_start.result, Some(LocalGpuCheck::Unavailable));
    app.gpu_start.manual = true;
    app.settings.ui_language = "da".to_owned();
    assert_readout(&mut app, "Ukendt GPU-hukommelse");
    assert!(start_enabled(&app));

    app.runtime_state = RuntimeState::Running;
    assert!(app.gpu_start_status().is_none());
    assert!(painted_texts_at(&mut app, egui::vec2(460.0, 210.0))
        .iter()
        .all(|label| !label.text.contains("Ukendt GPU-hukommelse")));
    assert!(!start_enabled(&app));
    app.gpu_start.manual = false;
    app.poll_gpu_start(&ctx);
    assert!(app.gpu_start.result.is_none());
    assert!(app.gpu_start.pending.is_none());
    assert!(app.gpu_start.refresh_at.is_none());

    app.runtime_state = RuntimeState::Stopped;
    app.poll_gpu_start(&ctx);
    assert!(app.gpu_start.pending.is_some());
    assert!(matches!(
        app.gpu_start_status(),
        Some(GpuStartStatus::Checking)
    ));

    std::fs::write(
        &config,
        r#"{"stt_backend":"openai","stt_provider":"openai","stt_model":"whisper-1"}"#,
    )
    .unwrap();
    app.gpu_start.refresh_selection();
    assert!(app.gpu_start.refresh_at.is_none());
    finish_sample(&mut app, |app| app.poll_gpu_start(&ctx));
    assert!(app.gpu_start.result.is_none());
    assert!(app.gpu_start_status().is_none());
    assert!(start_enabled(&app));
}

#[test]
fn a_probe_for_another_target_cannot_show_or_block_a_hosted_start() {
    let mut app = app();
    app.gpu_start
        .set_selection(Selection::Target(LocalGpuTarget::Whisper(4096)));
    let (sender, receiver) = mpsc::channel();
    app.gpu_start.pending = Some(PendingSample {
        receiver,
        obsolete: false,
    });
    app.gpu_start
        .set_selection(Selection::Target(LocalGpuTarget::Whisper(1024)));
    let now = Instant::now();
    sender
        .send((
            LocalGpuCheck::Capacity {
                required_mib: 4096,
                free_mib: Some(512),
            },
            now,
        ))
        .unwrap();
    app.gpu_start.complete_sample(now);
    assert_readout(&mut app, "Checking free GPU memory");
    assert!(!start_enabled(&app));
    assert!(app.gpu_start.result.is_none());

    app.gpu_start.set_selection(Selection::Hosted);
    assert!(start_enabled(&app));
    for compact in [false, true] {
        app.compact_mode = compact;
        assert!(painted_texts_at(&mut app, egui::vec2(1000.0, 640.0))
            .iter()
            .all(|label| !label.text.contains("512/4096")
                && !label.text.contains("Checking free GPU memory")));
    }
}

#[test]
fn unavailable_measurement_allows_authoritative_startup_check() {
    let mut app = app();
    app.gpu_start
        .set_selection(Selection::Target(LocalGpuTarget::Whisper(4096)));
    measured(
        &mut app,
        LocalGpuCheck::Capacity {
            required_mib: 4096,
            free_mib: None,
        },
    );
    assert_readout(&mut app, "No suitable GPU");
    assert!(!start_enabled(&app));

    measured(&mut app, LocalGpuCheck::Unavailable);
    assert_readout(&mut app, "checked at Start");
    assert!(start_enabled(&app));
}

#[test]
fn hviske_requires_the_selected_service_even_when_memory_is_free() {
    let mut app = app();
    app.gpu_start
        .set_selection(Selection::Target(LocalGpuTarget::Hviske(LocalModel::V6)));
    measured(
        &mut app,
        LocalGpuCheck::HviskeServiceNotReady {
            required_mib: 3584,
            free_mib: Some(8192),
        },
    );
    assert_readout(&mut app, "Hviske service not ready");
    assert_readout(&mut app, "8192/3584 MiB");
    assert!(!start_enabled(&app));

    measured(&mut app, LocalGpuCheck::HviskeServiceReady);
    assert_readout(&mut app, "Hviske service ready");
    assert!(start_enabled(&app));
}

#[test]
fn effective_whisper_target_honors_disk_then_ambient_model_and_path_override() {
    let _lock = crate::test_env_lock::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let _config = EnvVarGuard::set("VOICEPI_CONFIG", &path.to_string_lossy());
    let _model = EnvVarGuard::set("VOICEPI_MODEL", "large-v3");
    let _path = EnvVarGuard::remove(crate::ui::app::WHISPER_MODEL_PATH_ENV);
    let _device = EnvVarGuard::remove("VOICEPI_DEVICE");
    let _gpu = EnvVarGuard::remove(crate::whisper::GPU_ENV);
    let _backend = EnvVarGuard::set("VOICEPI_STT_BACKEND", "whisper");
    let mut app = app();
    app.settings.stt_backend = "openai".to_owned();
    app.settings.model = "small".to_owned();
    let target =
        |app: &WhisperDictateApp| effective_selection(&app.saved_worker_base_command().runtime);
    let preset = |name| {
        let entry = crate::whisper::model_manager::find(name).unwrap();
        Selection::Target(LocalGpuTarget::Whisper(entry.required_gpu_mib()))
    };

    assert!(target(&app) == preset("large-v3"));
    std::fs::write(
        &path,
        r#"{"stt_backend":"whisper","model":"large-v3-turbo"}"#,
    )
    .unwrap();
    assert!(target(&app) == preset("large-v3-turbo"));

    let _path = EnvVarGuard::set(crate::ui::app::WHISPER_MODEL_PATH_ENV, "custom-model.bin");
    assert!(
        target(&app) == Selection::Target(LocalGpuTarget::WhisperFile("custom-model.bin".into()))
    );
}

#[test]
fn only_the_actual_local_api_routes_have_gpu_targets() {
    let mut command = crate::runtime::WorkerCommand::from_runtime_pairs(
        "wd".into(),
        ".".into(),
        vec![
            ("VOICEPI_STT_BACKEND".into(), "openai".into()),
            ("VOICEPI_STT_MODEL".into(), "a-nemotron-model".into()),
            (
                "VOICEPI_STT_BASE_URL".into(),
                "grpc://localhost:50051".into(),
            ),
            ("VOICEPI_DEVICE".into(), "vulkan".into()),
        ],
    )
    .unwrap();
    command.runtime.set_stt_provider("nemotron");
    assert!(effective_selection(&command.runtime) == Selection::Hosted);

    command
        .set_runtime_value("VOICEPI_STT_BASE_URL", "inproc://nemotron")
        .unwrap();
    assert!(
        effective_selection(&command.runtime)
            == Selection::Target(LocalGpuTarget::Nemotron {
                model: "a-nemotron-model".into(),
                device: "vulkan".into(),
            })
    );

    command.runtime.set_stt_provider("custom");
    command
        .set_runtime_value("VOICEPI_STT_MODEL", LocalModel::V6.alias())
        .unwrap();
    assert!(
        effective_selection(&command.runtime)
            == Selection::Target(LocalGpuTarget::Hviske(LocalModel::V6))
    );
}

#[test]
fn runtime_error_takes_priority_in_the_bottom_bar_off_dictation() {
    let mut app = app();
    app.selected_tab = crate::ui::Tab::Speech;
    app.settings_status = "settings status".into();
    app.stt_api_key_status = "API key status".into();
    let error = "Whisper GPU memory: need 4096 MiB; 512 MiB free";
    let screen = egui::vec2(1000.0, 640.0);
    app.last_runtime_error = Some(error.into());
    assert!(painted_texts_at(&mut app, screen)
        .iter()
        .any(|label| label.rect.top() >= 600.0 && label.text.contains(error)));

    app.last_runtime_error = None;
    assert!(painted_texts_at(&mut app, screen)
        .iter()
        .any(|label| label.rect.top() >= 600.0 && label.text.contains("API key status")));
}
