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

#[test]
fn low_memory_is_visible_before_start_and_recovery_enables_start() {
    let mut app = app();
    app.gpu_start
        .set_selection(Selection::Target(LocalGpuTarget::Whisper(4096)));
    assert_readout(&mut app, "Checking free GPU memory");
    assert!(!start_enabled(&app));

    measured(
        &mut app,
        LocalGpuCheck::Capacity {
            required_mib: 4096,
            free_mib: Some(512),
        },
    );
    assert_readout(&mut app, "512/4096 MiB");
    assert_readout(&mut app, "too low");
    assert!(!start_enabled(&app));

    measured(
        &mut app,
        LocalGpuCheck::Capacity {
            required_mib: 4096,
            free_mib: Some(4096),
        },
    );
    assert_readout(&mut app, "4096/4096 MiB");
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
