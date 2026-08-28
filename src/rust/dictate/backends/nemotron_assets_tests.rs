use super::*;

#[test]
fn official_model_ids_resolve_to_pinned_assets() {
    assert_eq!(model_asset(NEMOTRON_MULTI_MODEL), Some(MULTI_MODEL));
    assert_eq!(model_asset(NEMOTRON_ENGLISH_MODEL), Some(ENGLISH_MODEL));
    assert_eq!(
        model_asset("nvidia/nemotron-asr-streaming"),
        Some(MULTI_MODEL)
    );
}

#[test]
fn well_known_gguf_filenames_are_downloadable() {
    assert_eq!(
        model_asset("C:/models/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf"),
        Some(MULTI_MODEL)
    );
    assert_eq!(
        model_asset("/tmp/nemotron-speech-streaming-en-0.6b.q8_0.gguf"),
        Some(ENGLISH_MODEL)
    );
}

#[test]
fn arbitrary_model_paths_are_not_replaced_by_a_network_download() {
    assert_eq!(model_asset("C:/models/my-custom-model.gguf"), None);
    assert_eq!(model_asset("my-model"), None);
}

#[test]
fn model_assets_have_pinned_https_urls_and_digests() {
    for asset in [MULTI_MODEL, ENGLISH_MODEL] {
        assert!(asset.url.starts_with("https://"));
        assert_eq!(asset.sha256.len(), 64);
        assert!(asset.size_bytes > 100_000_000);
    }
}

#[test]
fn cache_root_is_namespaced_by_runtime_version() {
    let root = cache_root().expect("test environment has a user cache");
    assert!(root.ends_with(
        Path::new("whisper-dictate")
            .join("nemotron")
            .join(RUNTIME_VERSION)
    ));
}

#[test]
fn runtime_archive_is_pinned_for_each_supported_platform() {
    for asset in [RUNTIME_CPU, RUNTIME_VULKAN] {
        assert!(asset
            .url
            .starts_with("https://github.com/NVIDIA/NeMo-Speech.cpp/"));
        assert_eq!(asset.sha256.len(), 64);
        assert!(asset.filename.contains(RUNTIME_VERSION));
        assert!(!asset.library_filename.is_empty());
    }
}
