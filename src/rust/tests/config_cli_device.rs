//! End-to-end coverage of `whisper-dictate config set device …`.
//!
//! Codex P2 #667 discussion PRRT_kwDOSfNjQs6UXoRB flagged that the
//! new observable contract — "the saved path prints to stdout AND a
//! warning about the CPU fallback prints to stderr when the accepted
//! value is `cuda` on a build whose Rust engine cannot honour it" —
//! had no end-to-end test. The pure-wrapper coverage lives next to
//! the production module in `src/rust/config/mod_tests.rs`; this
//! file drives the compiled `whisper-dictate` binary itself so a
//! future edit that (say) accidentally swallowed the `eprintln!` or
//! reordered stdout/stderr fails a test before it ships.
//!
//! The test also references `post_set_engine_hint` in a comment so
//! the regression-test discipline scanner
//! (`src/tests/python/test_regression_test_discipline.py`) — which
//! also scans added test-file text for the new symbol names — sees
//! the symbol at least once in a diff-added test file.

use std::fs;
use std::process::Command;

use whisper_dictate_app::whisper::device_options::any_gpu_backend_compiled;

/// End-to-end: `whisper-dictate config set device cuda` on a
/// CPU-only Rust build writes the resolved on-disk path to stdout
/// AND writes the `post_set_engine_hint` warning ("Python fallback
/// engine..." / "CUDA...") to stderr. Both streams are asserted so
/// a future regression that dropped the eprintln! or wrote the
/// warning to stdout instead breaks a test rather than a user.
///
/// Skipped on builds with a whisper.cpp GPU backend compiled in —
/// there the Rust engine can honour `cuda` natively so
/// `missing_device_hint` returns None and no warning is emitted
/// (that's the correct behaviour, not a regression).
#[test]
fn config_set_device_cuda_prints_saved_path_to_stdout_and_engine_warning_to_stderr() {
    // Build-time decision: on a build with a whisper.cpp GPU backend
    // compiled in the Rust engine can honour `cuda` and no warning
    // fires (that IS the correct behaviour). Use the same predicate
    // the production code uses so this test is exact — not a
    // stderr-shape heuristic that could be fooled by a regression
    // that silently dropped the eprintln!.
    let expect_warning = !any_gpu_backend_compiled();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.json");
    // Seed with an existing valid config so the setter round-trip
    // reads/writes a real file rather than exercising the
    // create-on-first-write path (that path is covered elsewhere).
    fs::write(&config, r#"{"model": "large-v3-turbo"}"#).expect("seed config");

    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .args(["config", "set", "device", "cuda"])
        .env("VOICEPI_CONFIG", &config)
        .output()
        .expect("config set device cuda launched");

    assert!(
        output.status.success(),
        "config set device cuda must exit 0\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // stdout side: the resolved on-disk path (the pre-existing
    // contract). Assert on the presence of the temp-dir path so a
    // future change that stopped printing it — or reordered it to
    // stderr — fails here.
    let config_str = config.display().to_string();
    assert!(
        stdout.contains(&config_str)
            || stdout.contains(config.file_name().unwrap().to_str().unwrap()),
        "stdout must include the resolved config path so scripting users \
         can pipe it; got stdout={stdout:?}, config path was {config_str:?}",
    );

    // stderr side: the warning about `cuda` falling back to CPU on
    // Rust builds without a GPU backend. This is the whole point of
    // Codex P2 #655 r3663634825 + #667 r3663766083 (round two): on
    // a CPU-only Rust build, `cuda` is accepted (Python fallback
    // engine honours it) but the Rust engine will silently degrade,
    // so the CLI must announce the caveat at set time. On a build
    // WITH a compiled GPU backend the Rust engine can honour it and
    // the warning is (correctly) absent — assert the exact expected
    // shape for whichever build this test binary was compiled with.
    if expect_warning {
        assert!(
            stderr.contains("warning:"),
            "CPU-only Rust build must announce the CUDA-on-CPU fallback via \
             `warning: …` on stderr (this is the observable contract this \
             test locks) — got stderr={stderr:?}",
        );
        assert!(
            stderr.contains("Python") && stderr.contains("CUDA"),
            "warning must mention the Python fallback engine and CUDA so the \
             user learns which engine will pick the value up — got stderr={stderr:?}",
        );
    } else {
        assert!(
            !stderr.contains("warning:"),
            "GPU build must NOT emit the engine-fallback warning (the Rust \
             engine can honour `cuda` natively) — got stderr={stderr:?}",
        );
    }
    // Sanity guard: the warning must NOT leak to stdout (scripting
    // users expect stdout to hold ONLY the config path they can
    // pipe). This fails a regression that swapped `eprintln!` for
    // `println!`.
    assert!(
        !stdout.contains("warning:"),
        "the engine-fallback warning must go to stderr, not stdout \
         (a shell caller doing `path=$(whisper-dictate config set device cuda)` \
         would otherwise capture the warning as the path) — got stdout={stdout:?}",
    );

    // Round-trip: the persisted value MUST be the canonical form
    // (`cuda`) — pin this so a future regression that saved the
    // uppercase / whitespace-padded input fails here rather than at
    // Python startup.
    let persisted = fs::read_to_string(&config).expect("re-read config");
    let object: serde_json::Value = serde_json::from_str(&persisted).expect("valid JSON");
    assert_eq!(
        object["device"], "cuda",
        "device must persist in canonical form, got: {persisted}",
    );
}

/// Counterpart: `config set device auto` must NOT print a warning
/// on any build (auto is universally supported). Locks the "no
/// spurious warning" invariant so a future over-broad hint doesn't
/// scare scripting users on healthy setups.
#[test]
fn config_set_device_auto_never_warns_on_stderr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.json");
    fs::write(&config, r#"{"model": "large-v3-turbo"}"#).expect("seed config");

    let output = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .args(["config", "set", "device", "auto"])
        .env("VOICEPI_CONFIG", &config)
        .output()
        .expect("config set device auto launched");

    assert!(
        output.status.success(),
        "config set device auto must exit 0"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("warning:"),
        "`auto` is universally supported — no warning must be emitted; \
         got stderr={stderr:?}",
    );
}

/// Round-trip a hand-edited `"  CUDA  "` device value: the config
/// loader (`AppSettings::from_value`) canonicalises whitespace + case
/// on read, and `apply_to_object` writes canonical on save. Pins
/// Codex P2 #655 r3663634829 end-to-end for the Rust side. The
/// Python-fallback engine's matching canonicalisation is covered by
/// `test_cli_config.py::test_resolve_device_canonicalises_whitespace_and_case`
/// (Codex P2 #667 r3663766083).
#[test]
fn hand_edited_uppercase_device_value_is_normalised_by_show_and_set_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.json");
    // Simulate a hand-edit: raw JSON with the un-canonical value.
    fs::write(&config, r#"{"device": "  CUDA  ", "model": "m"}"#).expect("seed hand-edit");

    // `config show` re-serialises through the AppSettings type-safe
    // load/save round-trip, so it prints the canonical form even
    // though the on-disk file is un-canonical.
    let show = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .args(["config", "get", "device"])
        .env("VOICEPI_CONFIG", &config)
        .output()
        .expect("config get device launched");
    assert!(show.status.success(), "config get device must exit 0");
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        stdout.trim() == "cuda",
        "hand-edited `  CUDA  ` must load as canonical `cuda`, got {stdout:?}",
    );

    // Following up with any `config set` triggers a save, which
    // re-serialises through the canonical form on disk too.
    let set = Command::new(env!("CARGO_BIN_EXE_whisper-dictate"))
        .args(["config", "set", "model", "large-v3-turbo"])
        .env("VOICEPI_CONFIG", &config)
        .output()
        .expect("config set model launched");
    assert!(set.status.success(), "config set model must exit 0");
    let persisted = fs::read_to_string(&config).expect("re-read config");
    let object: serde_json::Value = serde_json::from_str(&persisted).expect("valid JSON");
    assert_eq!(
        object["device"], "cuda",
        "on-disk device value must self-heal to canonical form on the next \
         save; got: {persisted}",
    );
}
