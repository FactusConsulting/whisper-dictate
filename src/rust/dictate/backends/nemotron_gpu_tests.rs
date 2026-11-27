use std::sync::atomic::Ordering;

use crate::dictate::TranscribeBackend;
use crate::model_capacity::GpuMemoryShortfall;

use super::{
    active_runtime, check_gpu_capacity_with, driver_enabled_with, load_recognizer_with,
    max_local_gpu_total_mib, Accelerator, DeviceRequest, GpuCapacity, GpuInfo,
    NemotronLocalBackendConfig, NemotronLocalTranscribeBackend, TranscriptionGuards,
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

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn style_capacity_sizes_the_real_file_and_shares_one_nvidia_snapshot() {
    use std::cell::Cell;

    use crate::model_capacity::vulkan::Adapters;

    use super::{gpu_memory_with, probe_free_mib_with};

    let (_directory, config) = custom_model_config(DeviceRequest::Auto);
    // At 683 MiB the custom file crosses the 2048 MiB official-model floor.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&config.model_path)
        .unwrap()
        .set_len(683 * (1 << 20))
        .unwrap();
    let model = format!("  {}  ", config.model_request);
    let cuda_available = Accelerator::Cuda.available();
    let mut cases = vec![
        ("vulkan", true, 1300, 1300),
        ("vulkan", true, 2500, 1800),
        ("auto", false, 1300, 1300),
        ("auto", true, 1300, if cuda_available { 1800 } else { 1300 }),
    ];
    if cuda_available {
        cases.push(("cuda", true, 1300, 1800));
    }
    for (device, allow_cuda, vulkan_free, expected_free) in cases {
        let queried = Cell::new(false);
        let vulkan_probed = Cell::new(false);
        let result = gpu_memory_with(&model, device, |device_request, required_mib| {
            probe_free_mib_with(
                device_request,
                required_mib,
                |accel| accel != Accelerator::Cuda || allow_cuda,
                || {
                    assert_ne!(device, "cuda", "explicit CUDA must not probe Vulkan");
                    assert!(!vulkan_probed.replace(true));
                    Adapters::with_test_capacity(c"NVIDIA test adapter", 8192, vulkan_free)
                },
                || {
                    assert!(!queried.replace(true), "one snapshot per capacity check");
                    let mut matching = fake_gpu(8192, 1800);
                    matching.name = "NVIDIA test adapter".to_owned();
                    vec![matching, fake_gpu(8192, 1600), fake_gpu(2048, 2048)]
                },
            )
        })
        .expect("an existing custom model should have a capacity plan");
        assert_eq!(result, (2049, Some(expected_free)), "{device}");
        assert!(queried.get(), "{device}");
        assert_eq!(vulkan_probed.get(), device != "cuda");
    }
}

#[test]
fn startup_preflight_reports_typed_shortfall_only_for_an_eligible_adapter() {
    let (_directory, config) = custom_model_config(DeviceRequest::Auto);
    let check = |free| {
        check_gpu_capacity_with(&config, |device, required_mib| {
            assert_eq!(device, DeviceRequest::Auto);
            assert_eq!(required_mib, 2048);
            free
        })
    };

    let error = check(Some(2047)).expect_err("free memory is one MiB short");
    assert_eq!(
        error.downcast_ref::<GpuMemoryShortfall>(),
        Some(&GpuMemoryShortfall {
            required_mib: 2048,
            free_mib: 2047,
        })
    );
    assert!(check(Some(2048)).is_ok());
    let error = check(None).expect_err("no adapter has sufficient total memory");
    assert!(error.downcast_ref::<GpuMemoryShortfall>().is_none());
    assert!(error
        .to_string()
        .contains("supported GPU with at least 2048 MiB"));
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
    let expected = if Accelerator::Vulkan.available() {
        let shortfall = error
            .downcast_ref::<GpuMemoryShortfall>()
            .expect("a usable but busy GPU must report the shortfall");
        assert_eq!((shortfall.required_mib, shortfall.free_mib), (2048, 2047));
        &[Accelerator::Vulkan][..]
    } else {
        assert!(error.to_string().contains("verified GPU runtime"));
        &[]
    };
    assert_eq!(probes, expected);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn adapter_below_total_requirement_does_not_load_a_runtime_or_report_a_shortfall() {
    use crate::model_capacity::vulkan::Adapters;

    let (_directory, config) = custom_model_config(DeviceRequest::Explicit(Accelerator::Vulkan));
    let error = load_recognizer_with::<(), ()>(
        &config,
        |_| true,
        |_| GpuCapacity::Vulkan(Adapters::with_test_capacity(c"small adapter", 2047, 2047)),
        |_| panic!("the adapter cannot fit the model even when completely idle"),
        |_, _, _| panic!("no runtime was loaded"),
        |_, _, _| panic!("no suitable GPU was found"),
    )
    .expect_err("the adapter total is too small");
    assert!(error.downcast_ref::<GpuMemoryShortfall>().is_none());
    assert!(error.to_string().contains("verified GPU runtime"));
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn recognizer_creation_reports_a_typed_shortfall_if_free_vram_drops_after_preflight() {
    use crate::model_capacity::vulkan::Adapters;

    let (_directory, config) = custom_model_config(DeviceRequest::Explicit(Accelerator::Vulkan));
    let description = c"NVIDIA test adapter";
    let mut probe_count = 0;
    let selected = std::cell::Cell::new(false);
    let error = load_recognizer_with::<_, ()>(
        &config,
        |_| true,
        |accelerator| {
            assert_eq!(accelerator, Accelerator::Vulkan);
            probe_count += 1;
            let free = if selected.get() { 2047 } else { 2048 };
            GpuCapacity::Vulkan(Adapters::with_test_capacity(description, 8192, free))
        },
        Ok,
        |accelerator, required_mib, capacity| {
            assert_eq!(*accelerator, Accelerator::Vulkan);
            assert_eq!(required_mib, 2048);
            assert_eq!(capacity.capacity(description), Some((8192, 2048)));
            assert!(!selected.replace(true));
            Ok(0)
        },
        |_, _, _| panic!("the refreshed budget cannot admit the recognizer"),
    )
    .expect_err("the GPU became busy before recognizer creation");
    assert_eq!(probe_count, 2);
    assert!(selected.get());
    assert_eq!(
        error.downcast_ref::<GpuMemoryShortfall>(),
        Some(&GpuMemoryShortfall {
            required_mib: 2048,
            free_mib: 2047,
        })
    );
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
