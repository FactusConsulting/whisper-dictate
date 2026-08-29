use super::*;

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

    extract_runtime_if_missing(&archive, &destination, library_filename)
        .expect("complete destination should win");

    assert_eq!(
        fs::read(destination.join("bin").join(library_filename)).expect("read winner"),
        b"winner"
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
