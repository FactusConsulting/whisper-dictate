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
fn auto_reports_the_primary_vulkan_accelerator_until_cpu_fallback() {
    let expected = if cfg!(target_os = "macos") {
        "cpu"
    } else {
        "vulkan"
    };
    assert_eq!(primary_accel_label("auto", "unknown"), expected);
    assert_eq!(primary_accel_label("cpu", "cpu"), "cpu");
}

#[test]
fn missing_model_path_is_actionable_before_loading_the_library() {
    let error = config_from_settings(
        "missing-model.gguf",
        "cpu",
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
    let directory = tempfile::tempdir().expect("temporary library directory");
    let library = directory.path().join("nemo_speech_asr_c.dll");
    std::fs::write(&library, b"fixture").expect("write library fixture");
    let config = config_from_settings(
        "nvidia/nemotron-3.5-asr-streaming-0.6b",
        "cpu",
        None,
        None,
        Some(&library.display().to_string()),
        true,
        active_runtime(),
    )
    .expect("official model should be planned without a download");
    assert!(config.local_only);
    assert!(config
        .model_path
        .ends_with("nemotron-3.5-asr-streaming-0.6b.q8_0.gguf"));
    assert_eq!(config.library_path, library);
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
            library_path: PathBuf::from("fixture.dll"),
            gpu: -1,
            accel_label: "cpu",
            language: None,
            initial_prompt: None,
            local_only: true,
            model_request: "fixture.gguf".to_owned(),
            library_override: None,
            device: "cpu".to_owned(),
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
    let mut reload = backend.prompt_reload.as_ref().unwrap().lock().unwrap();
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
            library_path: PathBuf::from("fixture.dll"),
            gpu: -1,
            accel_label: "cpu",
            language: None,
            initial_prompt: Some("Project Aurora".to_owned()),
            local_only: true,
            model_request: "fixture.gguf".to_owned(),
            library_override: None,
            device: "cpu".to_owned(),
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
            library_path: PathBuf::from("fixture.dll"),
            gpu: -1,
            accel_label: "cpu",
            language: None,
            initial_prompt: None,
            local_only: true,
            model_request: "fixture.gguf".to_owned(),
            library_override: None,
            device: "cpu".to_owned(),
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
    assert_eq!(backend.config().device, "cpu");
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
fn explicit_accelerators_are_planned_without_loading_the_runtime() {
    let directory = tempfile::tempdir().expect("temporary accelerator assets");
    let model = directory.path().join("model.gguf");
    let library = directory.path().join("runtime.dll");
    std::fs::write(&model, b"model").expect("write model");
    std::fs::write(&library, b"runtime").expect("write runtime");

    for (device, gpu, label) in [
        ("cuda", 0, "cuda"),
        ("vulkan", 0, "vulkan"),
        ("future", 0, "unknown"),
    ] {
        let config = config_from_settings(
            &model.display().to_string(),
            device,
            None,
            None,
            Some(&library.display().to_string()),
            true,
            active_runtime(),
        )
        .expect("explicit assets only plan the backend");
        assert_eq!(config.gpu, gpu);
        assert_eq!(config.accel_label, label);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn local_backend_transcribes_through_the_hermetic_native_abi() {
    let directory = tempfile::tempdir().expect("temporary Nemotron backend directory");
    let library = super::super::nemotron_ffi::build_fixture_library(directory.path());
    let model = directory.path().join("fixture.gguf");
    std::fs::write(&model, b"fixture model").expect("write fixture model");
    let backend = NemotronLocalTranscribeBackend::new(
        NemotronLocalBackendConfig {
            model_path: model.clone(),
            library_path: library.clone(),
            gpu: -1,
            accel_label: "cpu",
            language: Some("auto".to_owned()),
            initial_prompt: Some("Vocabulary: Codex".to_owned()),
            local_only: true,
            model_request: model.display().to_string(),
            library_override: Some(library.display().to_string()),
            device: "cpu".to_owned(),
            runtime_active: active_runtime(),
        },
        None,
    );
    let pcm = [0.001_f32, 0.5]
        .into_iter()
        .cycle()
        .take(40)
        .flat_map(|amplitude| std::iter::repeat_n(amplitude, 480))
        .collect::<Vec<_>>();
    let result = backend
        .transcribe(&pcm, 16_000)
        .expect("fixture backend transcription");
    assert_eq!(result.text, "fixture transcript");
    assert_eq!(result.raw_text, "fixture transcript");
    assert_eq!(result.language, "en-US");
    assert_eq!(result.stt_accel, "cpu");
    assert_eq!(
        result.stt_impl,
        crate::dictate::provenance::STT_IMPL_NEMOTRON_LOCAL
    );
}
