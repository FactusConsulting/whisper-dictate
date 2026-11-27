use super::*;

fn check_whisper(
    target: &LocalGpuTarget,
    probe: impl FnOnce() -> Option<Vec<SpeechGpu>>,
) -> LocalGpuCheck {
    target.check_with(
        probe,
        |_, _| panic!("not a Nemotron target"),
        |_| panic!("not a Hviske target"),
        |_| panic!("not a Hviske target"),
    )
}

#[test]
fn whisper_file_plan_uses_the_current_length_not_the_filename() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ggml-large-v3.bin");
    let file = std::fs::File::create(&path).unwrap();
    let target = LocalGpuTarget::WhisperFile(path);

    assert_eq!(
        check_whisper(&target, || Some(Vec::new())),
        LocalGpuCheck::Capacity {
            required_mib: 1024,
            free_mib: None,
        }
    );

    // One byte above 512 MiB moves the model past the minimum VRAM tier.
    file.set_len(512 * (1 << 20) + 1).unwrap();
    assert_eq!(
        check_whisper(&target, || Some(Vec::new())),
        LocalGpuCheck::Capacity {
            required_mib: 1026,
            free_mib: None,
        }
    );
}

#[test]
fn missing_whisper_file_or_directory_never_probes_the_gpu() {
    let dir = tempfile::tempdir().unwrap();
    for path in [dir.path().join("missing.bin"), dir.path().to_owned()] {
        assert_eq!(
            check_whisper(&LocalGpuTarget::WhisperFile(path), || {
                panic!("not a model file")
            }),
            LocalGpuCheck::Unavailable
        );
    }
}

#[test]
fn whisper_plan_excludes_small_adapters_and_does_not_pool_free_memory() {
    let gpus = || {
        Some(vec![
            SpeechGpu {
                index: 0,
                total_mib: 3072,
                free_mib: 3072,
            },
            SpeechGpu {
                index: 1,
                total_mib: 8192,
                free_mib: 2048,
            },
            SpeechGpu {
                index: 2,
                total_mib: 6144,
                free_mib: 2048,
            },
        ])
    };
    assert_eq!(
        check_whisper(&LocalGpuTarget::Whisper(4096), gpus),
        LocalGpuCheck::Capacity {
            required_mib: 4096,
            free_mib: Some(2048),
        }
    );
    assert_eq!(
        check_whisper(&LocalGpuTarget::Whisper(8193), gpus),
        LocalGpuCheck::Capacity {
            required_mib: 8193,
            free_mib: None,
        }
    );
    assert_eq!(
        check_whisper(&LocalGpuTarget::Whisper(4096), || None),
        LocalGpuCheck::Unavailable
    );
}

#[test]
fn nemotron_plan_distinguishes_an_invalid_request_from_no_eligible_adapter() {
    let target = LocalGpuTarget::Nemotron {
        model: "custom.gguf".to_owned(),
        device: "cuda".to_owned(),
    };
    assert_eq!(
        target.check_with(
            || panic!("not a Whisper target"),
            |model, device| {
                assert_eq!((model, device), ("custom.gguf", "cuda"));
                Some((2048, None))
            },
            |_| panic!("not a Hviske target"),
            |_| panic!("not a Hviske target"),
        ),
        LocalGpuCheck::Capacity {
            required_mib: 2048,
            free_mib: None,
        }
    );
    // Device parsing fails before any GPU work, also in a Nemotron-enabled build.
    assert_eq!(
        LocalGpuTarget::Nemotron {
            model: "custom.gguf".to_owned(),
            device: "cpu".to_owned(),
        }
        .check(),
        LocalGpuCheck::Unavailable
    );
}

fn check_hviske(
    model: LocalModel,
    ready: impl FnOnce(LocalModel) -> bool,
    free: impl FnOnce(u32) -> Option<u32>,
) -> LocalGpuCheck {
    LocalGpuTarget::Hviske(model).check_with(
        || panic!("not a Whisper target"),
        |_, _| panic!("not a Nemotron target"),
        ready,
        free,
    )
}

#[test]
fn resident_hviske_does_not_need_to_fit_in_free_memory_again() {
    assert_eq!(
        check_hviske(
            LocalModel::V6,
            |model| model == LocalModel::V6,
            |_| { panic!("a running service already owns its model memory") }
        ),
        LocalGpuCheck::HviskeServiceReady
    );
}

#[test]
fn stopped_hviske_service_is_not_ready_even_when_the_model_fits() {
    assert_eq!(
        check_hviske(
            LocalModel::V53,
            |model| {
                assert_eq!(model, LocalModel::V53);
                false
            },
            |required_mib| {
                assert_eq!(required_mib, 6144);
                Some(8192)
            },
        ),
        LocalGpuCheck::HviskeServiceNotReady {
            required_mib: 6144,
            free_mib: Some(8192),
        }
    );
}
