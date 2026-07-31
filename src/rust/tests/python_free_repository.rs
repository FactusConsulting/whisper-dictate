//! Final repository guard for the native-only architecture.
//!
//! The production runtime was removed earlier; this check closes the last
//! loophole by making the absence of tracked Python files and active Python
//! execution callouts a testable release invariant.

mod common;

use common::repo_root;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn files_under(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in
        fs::read_dir(root).unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("read directory entry: {error}"))
            .path();
        if path.is_dir() {
            files_under(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

#[test]
fn no_python_files_are_tracked() {
    let output = Command::new("git")
        .args(["ls-files", "*.py"])
        .current_dir(repo_root())
        .output()
        .expect("git is available");
    assert!(output.status.success(), "git ls-files failed");
    let tracked = String::from_utf8_lossy(&output.stdout);
    assert!(
        tracked.trim().is_empty(),
        "Python files remain tracked:\n{tracked}"
    );
}

#[test]
fn active_automation_has_no_python_runtime_or_dependency_callouts() {
    let roots = [
        ".github/workflows",
        ".devcontainer",
        "docker",
        "packaging",
        "scripts",
        "nix",
    ];
    let forbidden = [
        "actions/setup-python@",
        "python -m",
        "python3 -m",
        "python3.14",
        "pip install",
        "pytest",
        "PyYAML",
        "src/python/",
        "requirements/",
    ];
    let mut violations = Vec::new();
    for root in roots {
        let mut files = Vec::new();
        files_under(&repo_root().join(root), &mut files);
        for path in files {
            let source = fs::read_to_string(&path).unwrap_or_default();
            for marker in forbidden {
                if source.contains(marker) {
                    violations.push(format!("{}: {marker}", path.display()));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "active automation still references Python:\n{}",
        violations.join("\n")
    );
}
