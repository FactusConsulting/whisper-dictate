use super::*;

use std::sync::{mpsc, Arc};
use std::time::Duration;

use tempfile::tempdir;

fn runtime_library_filename() -> &'static str {
    if cfg!(windows) {
        "nemo_speech_asr_c.dll"
    } else {
        "libnemo_speech_asr_c.so"
    }
}

fn make_runtime_archive(root: &Path, library_filename: &str) -> PathBuf {
    let source = root.join("runtime-source");
    fs::create_dir_all(&source).expect("create runtime fixture source");
    fs::write(source.join(library_filename), b"runtime fixture")
        .expect("write runtime fixture library");
    let archive = root.join(if cfg!(windows) {
        "runtime.zip"
    } else {
        "runtime.tar.gz"
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
                source.join(library_filename),
            )
            .env("VOICEPI_NEMOTRON_ARCHIVE", &archive)
            .status()
            .expect("start PowerShell archive fixture")
    } else {
        Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .args(["-C"])
            .arg(&source)
            .arg(library_filename)
            .status()
            .expect("start tar archive fixture")
    };
    assert!(status.success(), "runtime fixture archive failed: {status}");
    archive
}

#[test]
fn runtime_extraction_publishes_a_verified_archive() {
    let directory = tempdir().expect("temporary Nemotron runtime directory");
    let library_filename = runtime_library_filename();
    let archive = make_runtime_archive(directory.path(), library_filename);
    let destination = directory.path().join("runtime");

    extract_runtime(&archive, &destination, library_filename).expect("extract runtime fixture");

    assert_eq!(
        fs::read(find_named_file(&destination, library_filename).expect("published library"))
            .expect("read published library"),
        b"runtime fixture"
    );
    assert!(runtime_cache_verified(
        &destination,
        library_filename,
        TEST_ARCHIVE_SHA256
    ));
}

#[test]
fn runtime_cache_verification_rejects_modified_library_and_archive_marker() {
    let directory = tempdir().expect("temporary verified runtime directory");
    let library_filename = runtime_library_filename();
    let destination = directory.path().join("runtime");
    let library = destination.join("bin").join(library_filename);
    fs::create_dir_all(library.parent().expect("library parent")).expect("create runtime bin");
    fs::write(&library, b"verified runtime").expect("write runtime library");
    write_runtime_verification_marker(&destination, &library, TEST_ARCHIVE_SHA256)
        .expect("write runtime marker");

    assert!(runtime_cache_verified(
        &destination,
        library_filename,
        TEST_ARCHIVE_SHA256
    ));
    assert!(!runtime_cache_verified(
        &destination,
        library_filename,
        "different-archive"
    ));
    fs::write(&library, b"truncated").expect("corrupt cached library");
    assert!(!runtime_cache_verified(
        &destination,
        library_filename,
        TEST_ARCHIVE_SHA256
    ));
}

#[test]
fn runtime_extraction_keeps_a_complete_process_winner() {
    let directory = tempdir().expect("temporary runtime winner directory");
    let library_filename = runtime_library_filename();
    let archive = make_runtime_archive(directory.path(), library_filename);
    let destination = directory.path().join("runtime");
    fs::create_dir_all(destination.join("bin")).expect("create winner directory");
    fs::write(destination.join("bin").join(library_filename), b"winner")
        .expect("write winner library");
    write_runtime_verification_marker(
        &destination,
        &destination.join("bin").join(library_filename),
        TEST_ARCHIVE_SHA256,
    )
    .expect("write winner marker");

    extract_runtime_if_missing(
        &archive,
        &destination,
        library_filename,
        TEST_ARCHIVE_SHA256,
        &std::sync::atomic::AtomicBool::new(true),
    )
    .expect("complete destination should win");

    assert_eq!(
        fs::read(destination.join("bin").join(library_filename)).expect("read winner"),
        b"winner"
    );
}

#[test]
fn stopped_runtime_does_not_launch_archive_extraction() {
    let directory = tempdir().expect("temporary cancelled extraction directory");
    let destination = directory.path().join("runtime");
    let archive = directory.path().join("missing-runtime.tar.gz");
    let stopped = std::sync::atomic::AtomicBool::new(false);

    let error = extract_runtime_if_missing(
        &archive,
        &destination,
        runtime_library_filename(),
        TEST_ARCHIVE_SHA256,
        &stopped,
    )
    .expect_err("stopped runtime must cancel before extraction starts");

    assert!(error.to_string().contains("extraction cancelled"));
    assert!(!directory.path().read_dir().unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("runtime-partial")));
}

#[test]
fn running_archive_extractor_is_interrupted_when_runtime_stops() {
    let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let worker_active = Arc::clone(&active);
    let mut command = if cfg!(windows) {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ]);
        command
    } else {
        let mut command = Command::new("sleep");
        command.arg("30");
        command
    };
    let started = std::time::Instant::now();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        worker_active.store(false, std::sync::atomic::Ordering::Release);
    });

    let error = run_extraction_command(&mut command, &active)
        .expect_err("runtime stop must interrupt the extractor");
    stopper.join().expect("stopper exits");

    assert!(error.to_string().contains("extraction cancelled"));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "extractor cancellation must not wait for command completion"
    );
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

#[test]
fn simultaneous_materialization_keeps_the_shared_verified_archive() {
    let directory = tempdir().expect("temporary concurrent runtime directory");
    let library_filename = runtime_library_filename();
    let archive = make_runtime_archive(directory.path(), library_filename);
    let archive_sha256 = Box::leak(sha256_file(&archive).unwrap().into_boxed_str());
    let destination = directory.path().join("runtime");
    let library = destination.join(library_filename);
    let asset = super::super::RuntimeAsset {
        filename: "runtime-fixture",
        url: "https://invalid.example/runtime-fixture",
        sha256: archive_sha256,
        library_filename,
    };
    let active = Arc::new(std::sync::atomic::AtomicBool::new(true));

    let workers: Vec<_> = (0..2)
        .map(|_| {
            let destination = destination.clone();
            let archive = archive.clone();
            let library = library.clone();
            let active = Arc::clone(&active);
            std::thread::spawn(move || {
                super::super::ensure_runtime_asset_at(
                    &destination,
                    &archive,
                    &library,
                    asset,
                    false,
                    &active,
                )
            })
        })
        .collect();

    for worker in workers {
        assert_eq!(worker.join().expect("materializer exits").unwrap(), library);
    }
    assert!(
        archive.is_file(),
        "peer extractors share the cached archive"
    );
    assert!(runtime_cache_verified(
        &destination,
        library_filename,
        archive_sha256
    ));
}

#[test]
fn local_only_materialization_extracts_a_retained_verified_archive() {
    let directory = tempdir().expect("temporary retained runtime directory");
    let library_filename = runtime_library_filename();
    let archive = make_runtime_archive(directory.path(), library_filename);
    let archive_sha256 = Box::leak(sha256_file(&archive).unwrap().into_boxed_str());
    let destination = directory.path().join("runtime");
    let library = destination.join(library_filename);
    let asset = super::super::RuntimeAsset {
        filename: "runtime-fixture",
        url: "https://invalid.example/runtime-fixture",
        sha256: archive_sha256,
        library_filename,
    };
    let active = AtomicBool::new(true);

    let resolved = super::super::ensure_runtime_asset_at(
        &destination,
        &archive,
        &library,
        asset,
        true,
        &active,
    )
    .expect("retained verified archive must repair the runtime offline");

    assert_eq!(resolved, library);
    assert!(runtime_cache_verified(
        &destination,
        library_filename,
        archive_sha256
    ));
}

#[test]
fn delayed_repair_rechecks_winner_after_acquiring_publication_lock() {
    let directory = tempdir().expect("temporary serialized publication directory");
    let library_filename = runtime_library_filename();
    let destination = directory.path().join("runtime");
    let staging = directory.path().join("runtime-staging");
    fs::create_dir_all(&destination).expect("create invalid destination");
    fs::write(destination.join(library_filename), b"invalid").expect("write invalid runtime");
    fs::create_dir_all(&staging).expect("create candidate staging");
    fs::write(staging.join(library_filename), b"candidate").expect("write candidate runtime");
    write_runtime_verification_marker(
        &staging,
        &staging.join(library_filename),
        TEST_ARCHIVE_SHA256,
    )
    .expect("verify candidate staging");

    let held = acquire_runtime_publish_lock(&destination).expect("hold publication lock");
    let (started_tx, started_rx) = mpsc::channel();
    let worker_destination = destination.clone();
    let worker_staging = staging.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        publish_runtime(
            &worker_staging,
            &worker_destination,
            library_filename,
            TEST_ARCHIVE_SHA256,
            false,
        )
    });
    started_rx.recv().expect("repairer started");
    std::thread::sleep(Duration::from_millis(50));
    assert!(!worker.is_finished(), "repairer must wait for its peer");

    fs::remove_dir_all(&destination).expect("remove invalid runtime while locked");
    fs::create_dir_all(&destination).expect("create peer winner");
    let winner = destination.join(library_filename);
    fs::write(&winner, b"peer winner").expect("write peer winner");
    write_runtime_verification_marker(&destination, &winner, TEST_ARCHIVE_SHA256)
        .expect("verify peer winner");
    drop(held);

    worker
        .join()
        .expect("delayed repairer exits")
        .expect("verified peer wins");
    assert_eq!(fs::read(winner).expect("read peer winner"), b"peer winner");
    assert!(!staging.exists(), "losing staging directory is cleaned");
}
