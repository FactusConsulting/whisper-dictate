//! Repository-policy guards for the retired Python production runtime.
//!
//! These checks deliberately live beside the Rust integration tests so the
//! native-only packaging and launch contract does not depend on pytest.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::repo_root;

fn read_repo_file(path: &str) -> String {
    let full_path = repo_root().join(path);
    fs::read_to_string(&full_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", full_path.display()))
}

fn visit_files(root: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("read directory {}: {error}", root.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("read directory entry: {error}"));
        let path = entry.path();
        if path.is_dir() {
            visit_files(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

fn source_files_under(relative_root: &str) -> Vec<PathBuf> {
    let root = repo_root().join(relative_root);
    let mut files = Vec::new();
    visit_files(&root, &mut files);
    files
}

fn assert_not_contains(label: &str, source: &str, markers: &[&str]) {
    for marker in markers {
        assert!(
            !source.contains(marker),
            "{label} still contains retired marker {marker:?}"
        );
    }
}

#[test]
fn retired_product_payload_files_are_absent() {
    for relative in ["src/python", "requirements"] {
        let path = repo_root().join(relative);
        if !path.exists() {
            continue;
        }
        let payload = source_files_under(relative)
            .into_iter()
            .filter(|file| {
                file.extension().and_then(|ext| ext.to_str()) != Some("pyc")
                    && !file.components().any(|component| {
                        matches!(
                            component.as_os_str().to_str(),
                            Some("__pycache__" | ".pytest_cache")
                        )
                    })
            })
            .collect::<Vec<_>>();
        assert!(
            payload.is_empty(),
            "retired product payload exists under {relative}: {payload:?}"
        );
    }
}

#[test]
fn native_production_code_has_no_python_process_launch_or_runtime_markers() {
    let source = source_files_under("src/rust")
        .into_iter()
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                && !path
                    .components()
                    .any(|component| component.as_os_str() == "tests")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_tests.rs"))
        })
        .map(|path| {
            fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert_not_contains(
        "native production Rust",
        &source,
        &[
            "Command::new(\"python",
            "whisper_dictate.runtime",
            "VOICEPI_PYTHON",
            "VOICEPI_RUST_INJECTOR",
            "PYTHONPATH",
        ],
    );
}

#[test]
fn cli_and_ui_expose_no_retired_install_path() {
    let cli = read_repo_file("src/rust/cli.rs");
    let main = read_repo_file("src/rust/main.rs");
    let ui = source_files_under("src/rust/ui")
        .into_iter()
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_tests.rs"))
        })
        .map(|path| {
            fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let combined = format!("{cli}\n{main}\n{ui}");
    assert_not_contains(
        "CLI/UI",
        &combined,
        &[
            "InstallRepair",
            "run_install",
            "run_install_command",
            "Command::Install",
        ],
    );
    assert!(!regex::Regex::new(r"(?m)^\s*Install\s*(?:\{|,)")
        .expect("valid install enum guard regex")
        .is_match(&cli));
}

#[test]
fn active_docs_do_not_invoke_retired_install_command() {
    let docs = source_files_under("docs")
        .into_iter()
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("md")
                && !path
                    .components()
                    .any(|component| component.as_os_str() == "archive")
        })
        .map(|path| {
            fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !regex::Regex::new(r"(?m)^(?:\./)?whisper-dictate(?:\.exe)?\s+install\b")
            .expect("valid install documentation guard regex")
            .is_match(&docs)
    );
}

#[test]
fn windows_artifacts_package_native_payload_only() {
    let sources = [
        (
            "Inno installer",
            read_repo_file("packaging/windows/inno/whisper-dictate.iss"),
        ),
        (
            "local portable zip",
            read_repo_file("scripts/windows/build-installer.ps1"),
        ),
        (
            "CI portable zip",
            read_repo_file(".github/workflows/windows-installer-build.yml"),
        ),
    ];
    let forbidden = [
        "src\\python",
        "src/python",
        "requirements",
        "whisper_dictate.runtime",
        "VOICEPI_PYTHON",
        "PYTHONPATH",
    ];
    for (label, source) in sources {
        assert_not_contains(label, &source, &forbidden);
    }
}

#[test]
fn release_bundles_and_nix_have_no_product_python() {
    let release = read_repo_file(".github/workflows/release.yml");
    let nix = read_repo_file("nix/flake.nix");
    let forbidden = [
        "cp -r src/python",
        "cp -r requirements",
        "whisper_dictate.runtime",
        "python -m venv",
        "pip install -r",
    ];
    assert_not_contains("release workflow", &release, &forbidden);
    assert_not_contains("Nix flake", &nix, &forbidden);
    assert!(release.contains("foreach ($retired in @('src\\python', 'requirements'))"));
    assert!(release.contains("Retired payload was installed"));
}

#[test]
fn automation_has_no_retired_runtime_callouts() {
    let roots = ["scripts", "packaging", "nix", "docker", ".devcontainer"];
    let forbidden = [
        "whisper_dictate.runtime",
        "whisper_dictate.vp_",
        "VOICEPI_PYTHON",
        "VOICEPI_RUST_INJECTOR",
        "PYTHONPATH",
        "src/python",
        "src\\python",
        "python -m whisper_dictate",
        "python3 -m whisper_dictate",
        "requirements/cpu.txt",
        "requirements/gpu.txt",
    ];
    let mut violations = Vec::new();
    for root in roots {
        for path in source_files_under(root) {
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default();
            if ["md", "wav", "png", "pyc"].contains(&extension)
                || path
                    .components()
                    .any(|component| component.as_os_str() == "__pycache__")
            {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            for marker in forbidden {
                if source.contains(marker) {
                    violations.push(format!("{}: {marker}", path.display()));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "automation still calls retired runtime:\n{}",
        violations.join("\n")
    );
}
