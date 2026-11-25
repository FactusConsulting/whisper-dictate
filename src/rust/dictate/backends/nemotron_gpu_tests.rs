use std::sync::atomic::Ordering;

use crate::dictate::TranscribeBackend;

use super::{
    active_runtime, driver_enabled_with, load_recognizer_with, max_local_gpu_total_mib,
    Accelerator, DeviceRequest, GpuCapacity, GpuInfo, NemotronLocalBackendConfig,
    NemotronLocalTranscribeBackend, TranscriptionGuards,
};

fn custom_model_config(device: DeviceRequest) -> (tempfile::TempDir, NemotronLocalBackendConfig) {
    let directory = tempfile::tempdir().expect("temporary model fixture directory");
    let model_path = directory.path().join("fixture.gguf");
    std::fs::write(&model_path, b"fixture model").expect("write fixture model");
    let config = NemotronLocalBackendConfig {
        model_request: model_path.to_str().unwrap().to_owned(),
        model_path,
        language: None,
        initial_prompt: None,
        local_only: true,
        library_override: None,
        device,
        runtime_active: active_runtime(),
    };
    (directory, config)
}

fn fake_gpu(total_mb: u32, free_mb: u32) -> GpuInfo {
    GpuInfo {
        index: 0,
        name: "test adapter".to_owned(),
        total_mb,
        free_mb,
        source: "fixture",
    }
}

#[test]
fn driver_visibility_honors_cuda_disable_and_empty_device_lists() {
    use std::ffi::OsString;

    for (accelerator, variable, accepts_minus_one) in [
        (Accelerator::Vulkan, "GGML_VK_VISIBLE_DEVICES", true),
        (Accelerator::Cuda, "CUDA_VISIBLE_DEVICES", false),
    ] {
        let enabled = |value: Option<&str>| {
            driver_enabled_with(accelerator, |name| {
                assert_eq!(name, variable);
                value.map(OsString::from)
            })
        };
        assert!(enabled(None));
        assert!(!enabled(Some("")));
        assert!(enabled(Some("0,2")));
        assert_eq!(enabled(Some("-1")), accepts_minus_one);
    }
}

#[test]
fn local_gpu_total_skips_disabled_probes_and_uses_the_largest_total() {
    assert_eq!(
        max_local_gpu_total_mib(
            |_| false,
            || panic!("disabled Vulkan capacity queried"),
            || panic!("disabled CUDA capacity queried"),
        ),
        None
    );
    let capacity = max_local_gpu_total_mib(
        |_| true,
        || 4096,
        || vec![fake_gpu(4096, 4096), fake_gpu(8192, 0)],
    );
    let expected = if Accelerator::Cuda.available() {
        Some(8192)
    } else if Accelerator::Vulkan.available() {
        Some(4096)
    } else {
        None
    };
    assert_eq!(capacity, expected);
}

#[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
#[test]
fn explicit_cuda_has_no_other_candidate() {
    let device = DeviceRequest::parse(" CuDa ").expect("pinned CUDA runtime on this platform");
    assert_eq!(device.candidates(), [Some(Accelerator::Cuda), None]);
}

#[test]
fn stopped_runtime_refuses_eager_and_lazy_load_without_leaving_a_resident_model() {
    use crate::audio_dsp::FRAME_SAMPLES;

    let (_directory, config) = custom_model_config(DeviceRequest::Auto);
    config.runtime_active.store(false, Ordering::Release);
    let error = NemotronLocalTranscribeBackend::check_configuration(&config)
        .expect_err("a stopped runtime cannot load a model");
    assert!(error
        .to_string()
        .contains("cancelled because the runtime stopped"));

    let backend = NemotronLocalTranscribeBackend::new(config, None)
        .with_transcription_guards(TranscriptionGuards::from_lookup(|_| None));
    let pcm: Vec<f32> = [0.001, 0.5, 0.001, 0.5, 0.001, 0.5]
        .into_iter()
        .flat_map(|amplitude| std::iter::repeat_n(amplitude, FRAME_SAMPLES))
        .collect();
    assert!(!backend.model.is_loaded());
    let error = backend
        .transcribe(&pcm, 16_000)
        .expect_err("decode must report the cancelled lazy load");
    assert!(error
        .to_string()
        .contains("cancelled because the runtime stopped"));
    assert!(!backend.model.is_loaded());

    std::fs::remove_file(&backend.config().model_path).expect("remove fixture model");
    let error = backend
        .transcribe(&pcm, 16_000)
        .expect_err("the failure must not be cached by the lazy loader");
    assert!(error
        .to_string()
        .contains("Nemotron model file does not exist"));
    assert!(!backend.model.is_loaded());
}

#[test]
fn no_ready_gpu_refuses_to_open_a_runtime_or_create_a_recognizer() {
    let (_directory, config) = custom_model_config(DeviceRequest::Auto);
    let mut probes = Vec::new();
    let error = load_recognizer_with::<(), ()>(
        &config,
        |accelerator| accelerator == Accelerator::Vulkan,
        |accelerator| {
            probes.push(accelerator);
            GpuCapacity::Cuda(vec![fake_gpu(8192, 2047)])
        },
        |_| panic!("no runtime may load without enough total and free VRAM"),
        |_, _, _| panic!("no runtime was loaded"),
        |_, _, _| panic!("no suitable GPU was found"),
    )
    .expect_err("a busy or disabled GPU cannot admit a local model");
    assert!(error
        .to_string()
        .contains("at least 2048 MiB total and free VRAM"));
    let expected = if Accelerator::Vulkan.available() {
        &[Accelerator::Vulkan][..]
    } else {
        &[]
    };
    assert_eq!(probes, expected);
}

#[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
#[test]
fn auto_falls_through_failed_vulkan_and_rechecks_cuda_before_opening_the_model() {
    use std::cell::RefCell;

    let (_directory, config) = custom_model_config(DeviceRequest::Auto);
    let calls = RefCell::new(Vec::new());
    let result = load_recognizer_with(
        &config,
        |_| true,
        |accelerator| {
            let mut calls = calls.borrow_mut();
            let free = if calls.contains(&("select", accelerator)) {
                4096
            } else {
                8192
            };
            calls.push(("probe", accelerator));
            GpuCapacity::Cuda(vec![fake_gpu(8192, free)])
        },
        |accelerator| {
            calls.borrow_mut().push(("load", accelerator));
            if accelerator == Accelerator::Vulkan {
                anyhow::bail!("Vulkan fixture rejected by runtime verification");
            }
            Ok(accelerator)
        },
        |accelerator, required_mib, capacity| {
            calls.borrow_mut().push(("select", *accelerator));
            assert_eq!(required_mib, 2048);
            match capacity.capacity(c"test adapter") {
                Some((8192, 8192)) => Ok(6),
                Some((8192, 4096)) => Ok(7),
                other => panic!("unexpected probe snapshot: {other:?}"),
            }
        },
        |accelerator, path, gpu| {
            calls.borrow_mut().push(("create", accelerator));
            assert_eq!(path, config.model_path.as_path());
            assert_eq!(gpu, 7);
            Ok("fixture recognizer")
        },
    )
    .expect("CUDA is a fallback GPU, not a CPU fallback");
    assert_eq!(result, ("fixture recognizer", "cuda"));
    assert_eq!(
        calls.into_inner(),
        [
            ("probe", Accelerator::Vulkan),
            ("load", Accelerator::Vulkan),
            ("probe", Accelerator::Cuda),
            ("load", Accelerator::Cuda),
            ("select", Accelerator::Cuda),
            ("probe", Accelerator::Cuda),
            ("select", Accelerator::Cuda),
            ("create", Accelerator::Cuda),
        ]
    );
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn explicit_gpu_reports_the_create_error_without_falling_back() {
    let device = DeviceRequest::parse(" VuLkAn ").expect("pinned Vulkan runtime on this platform");
    let (_directory, config) = custom_model_config(device);
    let error = load_recognizer_with::<_, ()>(
        &config,
        |_| true,
        |accelerator| {
            assert_eq!(accelerator, Accelerator::Vulkan);
            GpuCapacity::Cuda(vec![fake_gpu(8192, 8192)])
        },
        Ok,
        |_, _, _| Ok(3),
        |_, _, _| anyhow::bail!("fixture initialization failure"),
    )
    .expect_err("explicit Vulkan may not fall back after initialization fails");
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("requires a verified GPU runtime"));
    assert!(diagnostic.contains("initialize local Nemotron on vulkan GPU 3"));
    assert!(diagnostic.contains("fixture initialization failure"));
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn cancellation_after_runtime_load_prevents_the_final_probe_and_model_creation() {
    let (_directory, config) = custom_model_config(DeviceRequest::Explicit(Accelerator::Vulkan));
    let mut probes = 0;
    let error = load_recognizer_with::<_, ()>(
        &config,
        |_| true,
        |_| {
            probes += 1;
            GpuCapacity::Cuda(vec![fake_gpu(8192, 8192)])
        },
        |accelerator| {
            config.runtime_active.store(false, Ordering::Release);
            Ok(accelerator)
        },
        |_, _, _| Ok(0),
        |_, _, _| panic!("no model may become resident after cancellation"),
    )
    .expect_err("cancellation must be checked after the runtime is ready");
    assert_eq!(probes, 1);
    assert!(error
        .to_string()
        .contains("cancelled because the runtime stopped"));
}
