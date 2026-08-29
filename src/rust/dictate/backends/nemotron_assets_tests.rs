use super::*;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::thread::{self, JoinHandle};

use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[cfg(windows)]
fn runtime_library_filename() -> &'static str {
    "nemo_speech_asr_c.dll"
}

#[cfg(not(windows))]
fn runtime_library_filename() -> &'static str {
    "libnemo_speech_asr_c.so"
}

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
fn existing_explicit_model_path_wins_even_when_its_filename_is_official() {
    let directory = tempdir().expect("temporary Nemotron model directory");
    let path = directory.path().join(MULTI_MODEL.filename);
    fs::write(&path, b"developer-provided model").expect("write explicit model");

    assert_eq!(
        model_path_for_request(&path.display().to_string()).unwrap(),
        path
    );
}

#[test]
fn arbitrary_model_paths_are_not_replaced_by_a_network_download() {
    assert_eq!(model_asset("C:/models/my-custom-model.gguf"), None);
    assert_eq!(model_asset("my-model"), None);
}

#[test]
fn explicit_model_and_runtime_assets_bypass_the_bootstrap_cache() {
    let directory = tempdir().expect("temporary explicit Nemotron assets");
    let model = directory.path().join("developer-model.gguf");
    let library = directory.path().join(runtime_library_filename());
    fs::write(&model, b"developer model").expect("write explicit model");
    fs::write(&library, b"developer runtime").expect("write explicit runtime");

    assert_eq!(
        ensure_model_path(&model.display().to_string(), true).expect("existing model"),
        model
    );
    assert_eq!(
        library_path_for_request(Some(&library.display().to_string()), "cpu")
            .expect("existing runtime"),
        library
    );
    assert_eq!(
        ensure_library_path(Some(&library.display().to_string()), "cpu", true)
            .expect("existing runtime"),
        library
    );
}

#[test]
fn missing_explicit_assets_are_actionable_without_downloading() {
    let directory = tempdir().expect("temporary missing Nemotron assets");
    let model = directory.path().join("missing-model.gguf");
    let library = directory.path().join(runtime_library_filename());

    let model_error = ensure_model_path(&model.display().to_string(), true)
        .expect_err("missing custom model must not be downloaded");
    assert!(model_error.to_string().contains("does not exist"));
    let library_error = ensure_library_path(Some(&library.display().to_string()), "cpu", true)
        .expect_err("missing explicit runtime must not be downloaded");
    assert!(library_error
        .to_string()
        .contains("override does not exist"));
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

#[test]
fn runtime_cache_paths_are_variant_specific_and_keep_archive_extensions() {
    let (_, cpu_archive, cpu_library) = runtime_paths(RUNTIME_CPU).expect("CPU cache paths");
    let (_, vulkan_archive, vulkan_library) =
        runtime_paths(RUNTIME_VULKAN).expect("Vulkan cache paths");
    assert_eq!(
        cpu_archive.file_name().and_then(|name| name.to_str()),
        Some(RUNTIME_CPU.filename)
    );
    assert_eq!(
        vulkan_archive.file_name().and_then(|name| name.to_str()),
        Some(RUNTIME_VULKAN.filename)
    );
    assert!(cpu_library.ends_with(Path::new("bin").join(RUNTIME_CPU.library_filename)));
    assert!(vulkan_library.ends_with(Path::new("bin").join(RUNTIME_VULKAN.library_filename)));
    if RUNTIME_CPU.filename != RUNTIME_VULKAN.filename {
        assert_ne!(cpu_library, vulkan_library);
    }
}

#[test]
fn only_auto_and_cpu_may_reuse_a_discovered_runtime() {
    assert!(may_reuse_discovered_runtime("auto"));
    assert!(may_reuse_discovered_runtime("CPU"));
    assert!(!may_reuse_discovered_runtime("vulkan"));
    assert!(!may_reuse_discovered_runtime("cuda"));
}

#[test]
fn complete_process_winner_handles_directory_not_empty_publish_race() {
    let directory = tempdir().expect("temporary runtime winner directory");
    let library_filename = runtime_library_filename();
    let destination = directory.path().join("runtime");
    fs::create_dir_all(destination.join("bin")).expect("create winner directory");
    fs::write(destination.join("bin").join(library_filename), b"winner")
        .expect("write winner library");

    let error = std::io::Error::from(std::io::ErrorKind::DirectoryNotEmpty);
    assert!(process_winner_published(
        &error,
        false,
        &destination,
        library_filename
    ));
    assert!(!process_winner_published(
        &error,
        true,
        &destination,
        library_filename
    ));
}

#[test]
fn model_verification_cache_keeps_the_expected_digest_in_its_key() {
    let directory = tempdir().expect("temporary model verification cache");
    let path = directory.path().join("fixture.gguf");
    let body = b"cached model verification";
    fs::write(&path, body).expect("write model fixture");

    verify_cached_model(&path, &sha256_hex(body)).expect("first verification");
    verify_cached_model(&path, &sha256_hex(body)).expect("cached verification");
    assert!(verify_cached_model(&path, &"0".repeat(64)).is_err());
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

/// Serve one deterministic response to the ureq client without contacting the
/// network. The fixture deliberately accepts exactly one request so a failed
/// download cannot leave a background server thread behind.
fn serve_once(status: u16, body: Vec<u8>) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local fixture server");
    let address = listener.local_addr().expect("fixture address");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept fixture request");
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request);
        let reason = match status {
            200 => "OK",
            403 => "Forbidden",
            _ => "Fixture",
        };
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(header.as_bytes())
            .expect("write fixture headers");
        stream.write_all(&body).expect("write fixture body");
    });
    (format!("http://{address}"), handle)
}

#[test]
fn verified_download_publishes_only_matching_bytes() {
    let directory = tempdir().expect("temporary Nemotron asset directory");
    let target = directory.path().join("models").join("fixture.gguf");
    let body = b"small Nemotron model fixture".to_vec();
    let (url, server) = serve_once(200, body.clone());

    download_verified(
        &url,
        &sha256_hex(&body),
        body.len() as u64,
        &target,
        "test model",
    )
    .expect("matching fixture download");
    server.join().expect("fixture server exits");

    assert_eq!(fs::read(&target).expect("published fixture"), body);
    assert!(!target.with_extension("partial").exists());
}

#[test]
fn verified_download_cleans_partial_after_hash_mismatch() {
    let directory = tempdir().expect("temporary Nemotron asset directory");
    let target = directory.path().join("fixture.gguf");
    let body = b"tampered fixture".to_vec();
    let (url, server) = serve_once(200, body);

    let error = download_verified(&url, &"0".repeat(64), 1, &target, "mismatched model")
        .expect_err("mismatched fixture must fail");
    server.join().expect("fixture server exits");

    assert!(error.to_string().contains("SHA-256 mismatch"));
    assert!(!target.exists());
    assert!(!target.with_extension("partial").exists());
}

#[test]
fn verified_download_reports_http_failure_without_publishing() {
    let directory = tempdir().expect("temporary Nemotron asset directory");
    let target = directory.path().join("fixture.gguf");
    let body = b"denied fixture".to_vec();
    let (url, server) = serve_once(403, body);

    let error = download_verified(&url, &"0".repeat(64), 0, &target, "denied model")
        .expect_err("HTTP failure must be returned");
    server.join().expect("fixture server exits");

    assert!(error.to_string().contains("HTTP 403"));
    assert!(!target.exists());
    assert!(!target.with_extension("partial").exists());
}

#[test]
fn stopped_runtime_cancels_asset_download_before_opening_the_network() {
    let directory = tempdir().expect("temporary cancelled Nemotron asset directory");
    let target = directory.path().join("fixture.gguf");
    let active = AtomicBool::new(false);

    let error = download_verified_while(
        "http://127.0.0.1:1/never-contacted",
        &"0".repeat(64),
        0,
        &target,
        "model",
        &active,
    )
    .expect_err("stopped runtime must cancel before its HTTP request");

    assert!(error.to_string().contains("runtime stopped"));
    assert!(!target.exists());
}

#[test]
fn verified_publication_keeps_an_existing_matching_process_winner() {
    let directory = tempdir().expect("temporary Nemotron asset directory");
    let target = directory.path().join("fixture.gguf");
    let partial = directory.path().join(".fixture.partial.other-process");
    let body = b"winner fixture";
    fs::write(&target, body).expect("write published winner");
    fs::write(&partial, body).expect("write losing partial");

    publish_verified_file(&partial, &target, &sha256_hex(body))
        .expect("matching published winner should be accepted");

    assert_eq!(fs::read(&target).expect("read winner"), body);
    assert!(!partial.exists(), "losing staging file must be cleaned");
}

#[test]
fn verified_publication_replaces_an_invalid_existing_target() {
    let directory = tempdir().expect("temporary Nemotron asset directory");
    let target = directory.path().join("fixture.gguf");
    let partial = directory.path().join("fixture.partial");
    let winner = b"verified replacement fixture";
    fs::write(&target, b"stale fixture").expect("write stale target");
    fs::write(&partial, winner).expect("write verified partial");

    publish_verified_file(&partial, &target, &sha256_hex(winner))
        .expect("verified partial replaces stale target");

    assert_eq!(fs::read(&target).expect("read replacement"), winner);
    assert!(!partial.exists(), "published partial must be consumed");
}

#[test]
fn staging_paths_are_unique_per_publication_attempt() {
    let target = Path::new("C:/cache/nemotron/model.gguf");
    let first = unique_sibling_path(target, "partial");
    let second = unique_sibling_path(target, "partial");

    assert_ne!(first, second);
    assert_eq!(first.parent(), target.parent());
    assert!(first
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("partial"));
}

#[test]
fn cached_digest_verification_accepts_matches_and_rejects_other_files() {
    let directory = tempdir().expect("temporary Nemotron asset directory");
    let target = directory.path().join("fixture.gguf");
    let body = b"cached fixture";
    fs::write(&target, body).expect("write cached fixture");

    verify_sha256(&target, &sha256_hex(body)).expect("matching cache digest");
    assert!(verify_sha256(&target, &"0".repeat(64)).is_err());
    let missing = directory.path().join("missing.gguf");
    assert!(verify_sha256(&missing, &sha256_hex(body)).is_err());
}
