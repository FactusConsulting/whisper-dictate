use super::*;

use std::net::TcpListener;
use std::thread::{self, JoinHandle};

use tempfile::tempdir;

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

#[cfg(windows)]
fn runtime_library_filename() -> &'static str {
    "nemo_speech_asr_c.dll"
}

#[cfg(not(windows))]
fn runtime_library_filename() -> &'static str {
    "libnemo_speech_asr_c.so"
}

fn make_runtime_archive(
    root: &Path,
    library_filename: &str,
    include_library: bool,
    payload: &[u8],
) -> PathBuf {
    let source = root.join(if include_library {
        "runtime-source"
    } else {
        "runtime-source-missing"
    });
    fs::create_dir_all(&source).expect("create runtime fixture source");
    let filename = if include_library {
        library_filename
    } else {
        "not-the-library.txt"
    };
    fs::write(source.join(filename), payload).expect("write runtime fixture");
    let archive = root.join(if cfg!(windows) {
        if include_library {
            "runtime.zip"
        } else {
            "runtime-missing.zip"
        }
    } else if include_library {
        "runtime.tar.gz"
    } else {
        "runtime-missing.tar.gz"
    });
    let status = if cfg!(windows) {
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "Compress-Archive -LiteralPath $env:VOICEPI_NEMOTRON_FIXTURE -DestinationPath $env:VOICEPI_NEMOTRON_ARCHIVE -Force",
            ])
            .env("VOICEPI_NEMOTRON_FIXTURE", source.join(filename))
            .env("VOICEPI_NEMOTRON_ARCHIVE", &archive)
            .status()
            .expect("start PowerShell archive fixture")
    } else {
        Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .args(["-C"])
            .arg(&source)
            .arg(filename)
            .status()
            .expect("start tar archive fixture")
    };
    assert!(status.success(), "runtime fixture archive failed: {status}");
    archive
}

#[test]
fn runtime_extraction_publishes_and_replaces_a_verified_archive() {
    let directory = tempdir().expect("temporary Nemotron runtime directory");
    let library_filename = runtime_library_filename();
    let archive = make_runtime_archive(
        directory.path(),
        library_filename,
        true,
        b"first runtime fixture",
    );
    let destination = directory.path().join("runtime");

    extract_runtime(&archive, &destination, library_filename).expect("extract runtime fixture");
    let published = find_named_file(&destination, library_filename).expect("published library");
    assert_eq!(
        fs::read(published).expect("read published library"),
        b"first runtime fixture"
    );

    let replacement_source = directory.path().join("runtime-source-replacement");
    fs::create_dir_all(&replacement_source).expect("create replacement source");
    fs::write(
        replacement_source.join(library_filename),
        b"replacement runtime fixture",
    )
    .expect("write replacement library");
    let replacement_archive = directory.path().join(if cfg!(windows) {
        "runtime-replacement.zip"
    } else {
        "runtime-replacement.tar.gz"
    });
    let status = if cfg!(windows) {
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "Compress-Archive -LiteralPath $env:VOICEPI_NEMOTRON_FIXTURE -DestinationPath $env:VOICEPI_NEMOTRON_ARCHIVE -Force",
            ])
            .env(
                "VOICEPI_NEMOTRON_FIXTURE",
                replacement_source.join(library_filename),
            )
            .env("VOICEPI_NEMOTRON_ARCHIVE", &replacement_archive)
            .status()
            .expect("start PowerShell replacement fixture")
    } else {
        Command::new("tar")
            .args(["-czf"])
            .arg(&replacement_archive)
            .args(["-C"])
            .arg(&replacement_source)
            .arg(library_filename)
            .status()
            .expect("start tar replacement fixture")
    };
    assert!(status.success(), "replacement archive failed: {status}");
    extract_runtime(&replacement_archive, &destination, library_filename)
        .expect("replace runtime fixture");
    let replaced = find_named_file(&destination, library_filename).expect("replaced library");
    assert_eq!(
        fs::read(replaced).expect("read replaced library"),
        b"replacement runtime fixture"
    );
}

#[test]
fn runtime_extraction_removes_staging_when_archive_has_no_library() {
    let directory = tempdir().expect("temporary Nemotron runtime directory");
    let library_filename = runtime_library_filename();
    let archive = make_runtime_archive(
        directory.path(),
        library_filename,
        false,
        b"missing library fixture",
    );
    let destination = directory.path().join("runtime");
    let error = extract_runtime(&archive, &destination, library_filename)
        .expect_err("archive without library must fail");

    assert!(error.to_string().contains("did not contain"));
    assert!(!destination.with_extension("partial").exists());
}

#[test]
fn runtime_extraction_cleans_staging_when_archive_cannot_be_read() {
    let directory = tempdir().expect("temporary Nemotron runtime directory");
    let library_filename = runtime_library_filename();
    let archive = directory.path().join(if cfg!(windows) {
        "missing.zip"
    } else {
        "missing.tar.gz"
    });
    let destination = directory.path().join("runtime");
    let error = extract_runtime(&archive, &destination, library_filename)
        .expect_err("missing archive must fail");

    assert!(error.to_string().contains("failed to extract"));
    assert!(!destination.with_extension("partial").exists());
}
