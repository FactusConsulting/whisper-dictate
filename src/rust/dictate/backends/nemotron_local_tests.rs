use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::*;

fn active_runtime() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

#[test]
fn auto_is_sent_to_multilingual_model_explicitly() {
    let path = Path::new("nemotron-3.5-asr-streaming-0.6b.q8_0.gguf");
    assert_eq!(language_for_model("", path), "auto");
    assert_eq!(language_for_model("auto", path), "auto");
    assert_eq!(language_for_model("da", path), "da-DK");
    assert_eq!(language_for_model("en", path), "en-US");
    assert_eq!(language_for_model("fr", path), "fr-FR");
}

#[test]
fn auto_is_pinned_to_english_for_english_checkpoint() {
    let path = Path::new("nemotron-speech-streaming-en-0.6b.q8_0.gguf");
    assert_eq!(language_for_model("", path), "en-US");
    assert_eq!(language_for_model("auto", path), "en-US");
}

#[test]
fn explicit_bcp47_locale_is_preserved() {
    let path = Path::new("model.gguf");
    assert_eq!(language_for_model("fr-FR", path), "fr-FR");
    assert_eq!(language_for_model("en_US", path), "en-US");
}

#[test]
fn auto_result_does_not_become_a_fake_language() {
    assert_eq!(language_result_label("auto"), None);
    assert_eq!(language_result_label("multi"), None);
    assert_eq!(language_result_label("da-DK"), Some("da".to_owned()));
}

#[test]
fn auto_tries_only_accelerators_in_priority_order() {
    assert_eq!(
        DeviceRequest::Auto.candidates(),
        [Some(Accelerator::Vulkan), Some(Accelerator::Cuda)]
    );
    assert_eq!(
        DeviceRequest::Explicit(Accelerator::Cuda).candidates(),
        [Some(Accelerator::Cuda), None]
    );
}

#[test]
fn absent_language_is_model_aware() {
    let mut backend = fixture_backend();
    assert_eq!(backend.effective_language().as_deref(), Some("auto"));

    backend.config.model_path = PathBuf::from("nemotron-speech-streaming-en-0.6b.q8_0.gguf");
    assert_eq!(backend.effective_language().as_deref(), Some("en-US"));
}

#[test]
fn missing_model_path_is_actionable_before_loading_the_library() {
    let error = config_from_settings(
        "missing-model.gguf",
        "auto",
        None,
        None,
        None,
        false,
        active_runtime(),
    )
    .expect_err("missing model must fail before a dynamic load");
    assert!(error.to_string().contains("model file does not exist"));
}

#[test]
fn config_plans_official_assets_without_bootstrapping_them() {
    let config = config_from_settings(
        "nvidia/nemotron-3.5-asr-streaming-0.6b",
        "auto",
        None,
        None,
        None,
        true,
        active_runtime(),
    )
    .expect("official model should be planned without a download");
    assert!(config.local_only);
    assert!(config
        .model_path
        .ends_with("nemotron-3.5-asr-streaming-0.6b.q8_0.gguf"));
}

#[test]
fn local_prompt_and_replacements_share_the_nemotron_dictionary() {
    use crate::dictionary::DictionaryProvider;

    let directory = tempfile::tempdir().unwrap();
    let dictionary = directory.path().join("nemotron-dictionary.json");
    std::fs::write(
        &dictionary,
        r#"{"terms":["Codex"],"replacements":{"cloud code":"Claude Code"}}"#,
    )
    .unwrap();
    let backend = NemotronLocalTranscribeBackend::new(
        NemotronLocalBackendConfig {
            model_path: PathBuf::from("fixture.gguf"),
            language: None,
            initial_prompt: None,
            local_only: true,
            model_request: "fixture.gguf".to_owned(),
            library_override: None,
            device: DeviceRequest::Auto,
            runtime_active: active_runtime(),
        },
        None,
    )
    .with_reloading_prompt_settings(crate::dictionary::RuntimeDictionarySettings::new(
        true,
        vec![dictionary],
        80,
        1_200,
    ));
    let prompt = backend.effective_prompt();
    let mut reload = backend.overrides.prompt_reload().unwrap().lock().unwrap();
    let (rewritten, changes) = reload
        .current()
        .apply_replacements("open cloud code")
        .unwrap();
    assert_eq!(prompt.0.as_deref(), Some("Vocabulary: Codex"));
    assert_eq!(prompt.1, ["Codex"]);
    assert_eq!(rewritten, "open Claude Code");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].from, "cloud code");
    assert_eq!(changes[0].to, "Claude Code");
    assert_eq!(changes[0].count, 1);
}

#[test]
fn static_terms_remain_individual_native_speech_context_phrases() {
    let backend = NemotronLocalTranscribeBackend::new(
        NemotronLocalBackendConfig {
            model_path: PathBuf::from("fixture.gguf"),
            language: None,
            initial_prompt: Some("Project Aurora".to_owned()),
            local_only: true,
            model_request: "fixture.gguf".to_owned(),
            library_override: None,
            device: DeviceRequest::Auto,
            runtime_active: active_runtime(),
        },
        None,
    )
    .with_static_prompt_terms(vec!["Codex".to_owned(), "Cloudflare".to_owned()]);

    assert_eq!(
        backend.effective_prompt(),
        (
            Some("Project Aurora".to_owned()),
            vec!["Codex".to_owned(), "Cloudflare".to_owned()]
        )
    );
}

fn fixture_backend() -> NemotronLocalTranscribeBackend {
    NemotronLocalTranscribeBackend::new(
        NemotronLocalBackendConfig {
            model_path: PathBuf::from("nemotron-3.5-asr-streaming-0.6b.q8_0.gguf"),
            language: None,
            initial_prompt: None,
            local_only: true,
            model_request: "fixture.gguf".to_owned(),
            library_override: None,
            device: DeviceRequest::Auto,
            runtime_active: active_runtime(),
        },
        None,
    )
}

#[test]
fn profile_overrides_update_guards_prompt_and_language_together() {
    use crate::dictate::TranscribeBackend;

    let backend =
        fixture_backend().with_transcription_guards(TranscriptionGuards::from_lookup(|_| None));
    let settings = std::collections::BTreeMap::from([
        ("initial_prompt".to_owned(), "Project Aurora".to_owned()),
        ("lang".to_owned(), "da".to_owned()),
        ("min_input_dbfs".to_owned(), "-70".to_owned()),
    ]);

    backend.apply_profile_overrides(&settings);

    assert_eq!(
        backend.effective_prompt().0.as_deref(),
        Some("Project Aurora")
    );
    assert_eq!(backend.effective_language().as_deref(), Some("da-DK"));
    assert_eq!(backend.effective_guards().thresholds.min_input_dbfs, -70.0);
    assert_eq!(backend.config().device, DeviceRequest::Auto);
}

#[test]
fn profile_model_override_is_deferred_deduplicated_and_reset() {
    use crate::dictate::TranscribeBackend;

    let backend = fixture_backend();
    let model = std::collections::BTreeMap::from([(
        "model".to_owned(),
        "alternate-nemotron.gguf".to_owned(),
    )]);

    backend.apply_profile_overrides(&model);
    backend.apply_profile_overrides(&model);
    assert_eq!(
        backend.overrides.warned_model().as_deref(),
        Some("alternate-nemotron.gguf")
    );

    backend.apply_profile_overrides(&std::collections::BTreeMap::new());
    assert!(backend.overrides.warned_model().is_none());
}

#[test]
fn quiet_audio_is_rejected_before_the_lazy_native_model_load() {
    use crate::dictate::TranscribeBackend;

    let backend =
        fixture_backend().with_transcription_guards(TranscriptionGuards::from_lookup(|_| None));
    let result = backend
        .transcribe(&vec![0.0; 16_000], 16_000)
        .expect("speech gate rejection is a successful empty result");

    assert!(result.text.is_empty());
    assert!(result
        .gate
        .as_deref()
        .is_some_and(|gate| gate.contains("too quiet")));
}

#[test]
fn cpu_is_rejected_before_planning_assets() {
    let error = config_from_settings(
        "missing-model.gguf",
        " CPU ",
        None,
        None,
        None,
        false,
        active_runtime(),
    )
    .expect_err("local Nemotron is GPU-only");
    assert!(error.to_string().contains("CPU inference is not allowed"));
}

#[test]
fn cuda_capacity_uses_adapter_name_and_conservative_duplicate_capacity() {
    let gpu = |index, name: &str, total_mb, free_mb| GpuInfo {
        index,
        name: name.to_owned(),
        total_mb,
        free_mb,
        source: "nvidia-smi",
    };
    let gpus = [
        gpu(5, "NVIDIA A", 4096, 3000),
        gpu(0, "NVIDIA B", 8192, 7900),
        gpu(9, "NVIDIA A", 4096, 1000),
    ];
    assert_eq!(cuda_capacity(&gpus, b"NVIDIA A"), Some((4096, 1000)));
    assert_eq!(cuda_capacity(&gpus, b"NVIDIA B"), Some((8192, 7900)));
    assert_eq!(cuda_capacity(&gpus, b"unrecognized"), None);
    assert_eq!(cuda_capacity(&[], b"NVIDIA A"), None);
}

#[test]
fn cuda_capacity_cannot_be_combined_across_adapters() {
    let capacity = GpuCapacity::Cuda(vec![
        GpuInfo {
            index: 0,
            name: "NVIDIA small".to_owned(),
            total_mb: 1900,
            free_mb: 1900,
            source: "nvidia-smi",
        },
        GpuInfo {
            index: 1,
            name: "NVIDIA busy".to_owned(),
            total_mb: 4096,
            free_mb: 1800,
            source: "nvidia-smi",
        },
    ]);
    assert!(!capacity.has_capacity(2048));
}

#[test]
fn larger_custom_gguf_requires_additional_vram() {
    assert_eq!(nemotron_required_mib(0), 2048);
    assert_eq!(nemotron_required_mib(742_000_000), 2086);
    assert_eq!(nemotron_required_mib(4 * 1024 * 1024 * 1024), 7168);
}

#[test]
fn shared_construction_rejects_an_unsupported_device() {
    let error = config_from_settings(
        "missing-model.gguf",
        "future-accelerator",
        None,
        None,
        None,
        true,
        active_runtime(),
    )
    .expect_err("all one-shot callers must share device validation");
    assert!(error.to_string().contains("not supported"));
    assert!(error.to_string().contains("future-accelerator"));
}

#[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
#[test]
fn shared_construction_rejects_cuda_without_a_pinned_runtime() {
    let error = config_from_settings(
        "missing-model.gguf",
        "cuda",
        None,
        None,
        None,
        true,
        active_runtime(),
    )
    .expect_err("CUDA must be rejected when this target has no pinned runtime");
    assert!(error.to_string().contains("not supported"));
}

#[path = "nemotron_gpu_tests.rs"]
mod gpu;
