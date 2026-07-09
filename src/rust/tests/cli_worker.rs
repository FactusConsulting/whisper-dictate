use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn help_uses_public_binary_name_even_when_binary_path_differs() {
    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("--help")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    // After issue #326 added top-level `--toggle-recording` / etc. flags,
    // clap renders `Usage: whisper-dictate [OPTIONS] [COMMAND]` instead of
    // `[COMMAND]`. Accept either shape so the public binary-name assertion
    // (the actual contract of this test) keeps holding regardless of how
    // many top-level flags exist.
    assert!(
        stdout.contains("Usage: whisper-dictate"),
        "expected public binary name in usage line, got: {stdout}"
    );
    assert!(!stdout.contains("Usage: whisper-dictate-app"));
}

#[test]
fn version_flag_prints_public_version_line() {
    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("--version")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.starts_with("whisper-dictate "));
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
        let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
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
fn format_text_helper_returns_structured_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
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
fn dictionary_runtime_helper_returns_prompt_terms_and_changes() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let dictionary = dir.path().join("dictionary.json");
    fs::write(
        &dictionary,
        r#"{"terms":["Codex","Claude Code"],"replacements":{"Cloud Code":"Claude Code"}}"#,
    )
    .unwrap();
    fs::write(
        &config,
        serde_json::json!({
            "dictionary": dictionary,
            "dictionary_max_terms": "1",
            "dictionary_prompt_chars": "1200"
        })
        .to_string(),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("dictionary-runtime")
        .env("VOICEPI_CONFIG", &config)
        .env("VOICEPI_DICTIONARY", &dictionary)
        .env("VOICEPI_DICTIONARY_ENABLED", "1")
        .env("VOICEPI_DICTIONARY_MAX_TERMS", "1")
        .env("VOICEPI_DICTIONARY_PROMPT_CHARS", "1200")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            serde_json::json!({
                "base_prompt": "Base prompt",
                "text": "Open Cloud Code"
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "dictionary-runtime failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["enabled"], true);
    assert_eq!(value["term_count"], 2);
    assert_eq!(value["replacement_count"], 1);
    assert_eq!(value["terms"], serde_json::json!(["Codex"]));
    assert_eq!(
        value["all_terms"],
        serde_json::json!(["Codex", "Claude Code"])
    );
    assert_eq!(value["prompt"], "Base prompt\nVocabulary: Codex");
    assert_eq!(value["text"], "Open Claude Code");
    assert_eq!(value["changes"][0]["from"], "Cloud Code");
    assert_eq!(value["changes"][0]["count"], 1);
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

    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
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

    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
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
fn redact_text_helper_reads_json_stdin_and_omits_values_from_text() {
    let output = command_with_stdin(
        &["redact-text"],
        &serde_json::json!({
            "text": "Kontakt Lars på lars@example.com.",
            "terms": ["Lars"],
        })
        .to_string(),
    );

    assert!(
        output.status.success(),
        "redact-text failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["text"], "Kontakt [[WD_TERM_2]] på [[WD_EMAIL_1]].");
    assert_eq!(value["redactions"][0]["kind"], "email");
    assert_eq!(value["redactions"][0]["value"], "lars@example.com");
    assert_eq!(value["redactions"][1]["kind"], "term");
    assert_eq!(value["redactions"][1]["value"], "Lars");
}

#[test]
fn privacy_helper_reports_local_only_backend_blocks_as_json() {
    let output = command_with_stdin(
        &["privacy"],
        &serde_json::json!({
            "action": "assert_backend",
            "local_only": true,
            "backend": "openai:gpt-4o-transcribe",
            "feature": "STT",
        })
        .to_string(),
    );

    assert!(
        output.status.success(),
        "privacy failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("VOICEPI_LOCAL_ONLY=1 blocks STT backend"));
}

// Wave 8 Part 2: `worker_failure_does_not_print_rust_backtrace` and
// `foreground_worker_inherits_utf8_stdio_envs` used to build a fake
// Python worker under `<tmp>/src/python/whisper_dictate/runtime.py`,
// point `VOICEPI_PYTHON` at a real interpreter, and drive
// `whisper-dictate doctor` against it to assert the runtime shelled
// out with the right stdio envs and surfaced non-zero exit codes
// cleanly. The Python fallback (and `run_foreground`) was deleted in
// v1.20; `doctor` now prints a "moved to worker-rust" in-process
// message and returns Ok, so both tests are moot.

#[test]
fn doctor_prints_moved_message_and_succeeds() {
    // Confirm the Wave 8 Part 2 doctor stub is wired up: it must exit
    // 0 (so users' packaging scripts that check the return code stay
    // green) and mention the Python removal + the surviving Rust
    // diagnostics.
    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .arg("doctor")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "doctor stub failed\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("removed in v1.20"),
        "expected v1.20 removal note in stdout, got: {stdout}"
    );
}

fn command_with_stdin(args: &[&str], stdin: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();

    child.wait_with_output().unwrap()
}

// `test_python`, `find_on_path`, `python_candidates` were the Python-
// launcher discovery helpers used by the deleted Wave 8 Part 2 Python
// worker regression tests. Removed alongside their callers.
