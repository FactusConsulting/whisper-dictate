use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

const WD: &str = env!("CARGO_BIN_EXE_wd");

#[test]
fn canonical_binary_uses_wd_in_help() {
    let output = Command::new(WD).arg("--help").output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("Usage: wd [COMMAND]"));
    assert!(!stdout.contains("Usage: whisper-dictate-app"));
}

#[test]
fn run_help_succeeds_on_a_reduced_native_build() {
    let output = Command::new(WD)
        .args(["run", "--help"])
        .env_remove("VOICEPI_DICTATE_ENGINE")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: wd run"));
    assert!(!stdout.contains("native dictation features are not compiled"));
}

#[test]
fn version_flag_prints_public_version_line() {
    let output = Command::new(WD).arg("--version").output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.starts_with("wd "));
}

#[test]
fn transcribe_file_cli_preserves_path_and_stderr_contract() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing recording.wav");
    let output = Command::new(WD)
        .arg("transcribe-file")
        .arg(&missing)
        .arg("--json")
        .env("VOICEPI_CONFIG", dir.path().join("missing-config.json"))
        .output()
        .expect("launch installed controller command");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "errors must not pollute stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("input file does not exist"),
        "missing-file diagnostic absent: {stderr}"
    );
    assert!(
        stderr.contains(&missing.display().to_string()),
        "input path was not preserved: {stderr}"
    );
}

#[test]
fn transcribe_file_cli_materializes_canonical_cloud_backend() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("valid recording.wav");
    let config = dir.path().join("config.json");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
    writer.write_sample(0_i16).unwrap();
    writer.finalize().unwrap();
    fs::write(
        &config,
        r#"{"stt_backend":" OPENAI ","stt_model":"test-model","local_only":true}"#,
    )
    .unwrap();

    let output = Command::new(WD)
        .arg("transcribe-file")
        .arg(&wav)
        .env("VOICEPI_CONFIG", &config)
        .env_remove("VOICEPI_STT_API_KEY")
        .env_remove("GROQ_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("launch installed controller command");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "errors must not pollute stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires a saved API key") || stderr.contains("cloud backend rejected"),
        "cloud backend was not reached safely: {stderr}"
    );
    assert!(
        !stderr.contains("unsupported stt_backend"),
        "accepted backend spelling was not canonicalized: {stderr}"
    );
}

#[test]
fn calibrate_file_cli_emits_native_json_report() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("calibration sample.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
    for sample in
        std::iter::repeat_n(33_i16, 480 * 4).chain(std::iter::repeat_n(6_500_i16, 480 * 4))
    {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();

    let output = Command::new(WD)
        .arg("calibrate-file")
        .arg(&wav)
        .arg("--json")
        .output()
        .expect("launch native calibration command");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["event"], "mic_calibration");
    assert_eq!(report["status"], "pass");
    assert_eq!(report["source_file"], wav.to_string_lossy().as_ref());
    assert!(report["snr_db"].as_f64().unwrap() > 15.0);
    assert_eq!(report["recommended"]["VOICEPI_TARGET_DBFS"], "-20");
}

#[test]
fn rust_application_startup_smoke_commands_do_not_crash() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let dictionary = dir.path().join("dictionary.json");
    fs::write(&dictionary, r#"{"terms":["Codex"],"replacements":{}}"#).unwrap();
    fs::write(
        &config,
        serde_json::json!({
            "dictionary": dictionary,
            "format_commands": "da"
        })
        .to_string(),
    )
    .unwrap();

    let cases: &[(&[&str], &str)] = &[
        (&["--version"], "version"),
        (&["--help"], "help"),
        (&["config", "show"], "config show"),
        (&["dictionary", "status"], "dictionary status"),
        (
            &[
                "format-text",
                "--text",
                "første komma",
                "--command-set",
                "da",
            ],
            "format helper",
        ),
    ];

    for (args, label) in cases {
        let output = Command::new(WD)
            .args(*args)
            .env("VOICEPI_CONFIG", &config)
            .output()
            .unwrap_or_else(|err| panic!("{label} failed to launch: {err}"));
        assert!(
            output.status.success(),
            "{label} crashed or exited unsuccessfully\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn export_config_redacts_secrets_by_default_and_requires_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    fs::write(&config, r#"{"lang":"da","model":"small"}"#).unwrap();

    let run = |include: bool| {
        let mut command = Command::new(WD);
        command
            .arg("export-config")
            .env("VOICEPI_CONFIG", &config)
            .env("VOICEPI_STT_API_KEY", "secret-with-'quote")
            .env_remove("VOICEPI_POST_API_KEY")
            .env_remove("GROQ_API_KEY")
            .env_remove("OPENAI_API_KEY");
        if include {
            command.arg("--include-secrets");
        }
        command.output().unwrap()
    };

    let hidden = run(false);
    assert!(hidden.status.success());
    let hidden_text = String::from_utf8(hidden.stdout).unwrap();
    assert!(hidden_text.contains(r#""lang": "da""#));
    assert!(hidden_text.contains("$env:VOICEPI_STT_API_KEY = '***'"));
    assert!(!hidden_text.contains("secret-with-"));

    let shown = run(true);
    assert!(shown.status.success());
    let shown_text = String::from_utf8(shown.stdout).unwrap();
    assert!(shown_text.contains("$env:VOICEPI_STT_API_KEY = 'secret-with-''quote'"));
    assert!(shown_text.contains("export VOICEPI_STT_API_KEY='secret-with-'\\''quote'"));
}

#[test]
fn setup_cli_accepts_scripted_input_and_writes_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("nested").join("config.json");
    let mut child = Command::new(WD)
        .arg("setup")
        .env("VOICEPI_CONFIG", &config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch native setup");
    let mut script = String::from("\nlarge-v3\n");
    script.push_str(&"\n".repeat(20));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Wrote config to:"));
    assert!(stdout.contains("# === PowerShell ==="));
    assert!(stdout.contains("# === bash ==="));
    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(saved["model"], "large-v3");
}

#[test]
fn format_text_helper_returns_structured_json() {
    let output = Command::new(WD)
        .args([
            "format-text",
            "--text",
            "første komma ny linje andet punktum",
            "--command-set",
            "da",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "format-text failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["enabled"], true);
    assert_eq!(value["changed"], true);
    assert_eq!(value["command_set"], "da");
    assert_eq!(value["text"], "første,\nandet.");
    let applied = value["applied"].as_array().unwrap();
    assert!(applied
        .iter()
        .any(|item| item["command"] == "komma" && item["count"] == 1));
}

#[test]
fn dictionary_build_from_corpus_emits_json_preview_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("corpus.json");
    fs::write(
        &manifest,
        r#"{
          "version": 1,
          "items": [
            {"id":"da-tech-001","language":"da","category":"mixed_technical",
             "text":"Skift backend til Parakeet.","terms":["Parakeet"]},
            {"id":"da-prod-001","language":"da","category":"product_names",
             "text":"Claude Code og Codex.","terms":["Claude Code","Codex"]}
          ]
        }"#,
    )
    .unwrap();
    let dict = dir.path().join("dictionary.json");

    let output = Command::new(WD)
        .args(["dictionary", "build-from-corpus", "--benchmark-corpus"])
        .arg(&manifest)
        .arg("--dictionary")
        .arg(&dict)
        .arg("--json")
        .output()
        .expect("subcommand failed to launch");

    assert!(
        output.status.success(),
        "build-from-corpus exited non-zero\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The dictionary file MUST NOT exist after a preview run — that contract
    // is what makes the preview safe to wire into automation without a
    // separate dry-run flag.
    assert!(!dict.exists(), "preview must not write the dictionary file");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "build-from-corpus");
    assert_eq!(value["applied"], false);
    let added = value["added"].as_array().unwrap();
    assert!(added.iter().any(|t| t == "Parakeet"));
    assert!(added.iter().any(|t| t == "Claude Code"));
}

#[test]
fn dictionary_suggest_terms_emits_json_preview_with_already_in_dictionary_flag() {
    let dir = tempfile::tempdir().unwrap();
    let dict = dir.path().join("dictionary.json");
    fs::write(&dict, r#"{"terms":["deploy"],"replacements":{}}"#).unwrap();
    let jsonl = dir.path().join("results.jsonl");
    fs::write(
        &jsonl,
        "{\"corpus_id\":\"a\",\"term_misses\":[\"merge\",\"deploy\"]}\n\
         {\"corpus_id\":\"b\",\"term_misses\":[\"NVIDIA Parakeet\"]}\n",
    )
    .unwrap();

    let output = Command::new(WD)
        .args(["dictionary", "suggest-terms"])
        .arg(&jsonl)
        .arg("--dictionary")
        .arg(&dict)
        .arg("--json")
        .output()
        .expect("subcommand failed to launch");

    assert!(
        output.status.success(),
        "suggest-terms exited non-zero\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "suggest-from-benchmark-misses");
    assert_eq!(value["applied"], false);
    let new_terms = value["new_terms"].as_array().unwrap();
    assert!(new_terms.iter().any(|t| t == "merge"));
    assert!(new_terms.iter().any(|t| t == "NVIDIA Parakeet"));
    // "deploy" is already in the dictionary, so it MUST be reported with
    // already_in_dictionary=true and excluded from new_terms.
    assert!(!new_terms.iter().any(|t| t == "deploy"));
    let suggestions = value["suggestions"].as_array().unwrap();
    let deploy = suggestions
        .iter()
        .find(|s| s["term"] == "deploy")
        .expect("deploy must be in suggestions");
    assert_eq!(deploy["already_in_dictionary"], true);
}

#[test]
fn retired_python_engine_fails_without_running_a_worker_or_backtrace() {
    // A deliberately invalid app root proves the migration error happens
    // before any worker discovery or child-process launch can occur.
    let output = Command::new(WD)
        .arg("run")
        .env("VOICEPI_DICTATE_ENGINE", "python")
        .env("VOICEPI_APP_ROOT", "Z:\\path-that-must-not-exist")
        .env("RUST_BACKTRACE", "1")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success());
    assert!(stdout.is_empty());
    assert!(stderr.contains("VOICEPI_DICTATE_ENGINE=python is no longer supported"));
    assert!(stderr.contains("remove the variable"));
    assert!(!stderr.contains("Stack backtrace"));
}

#[test]
fn unknown_engine_fails_without_attempting_legacy_worker_discovery() {
    let output = Command::new(WD)
        .arg("run")
        .env("VOICEPI_DICTATE_ENGINE", "mojo")
        .env("VOICEPI_APP_ROOT", "Z:\\path-that-must-not-exist")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("unknown VOICEPI_DICTATE_ENGINE"));
    assert!(stderr.contains("mojo"));
}
