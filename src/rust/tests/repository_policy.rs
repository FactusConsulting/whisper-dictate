//! Repository-policy checks that do not require a Python interpreter.
//!
//! These are intentionally small, source-oriented guards.  Runtime behaviour
//! belongs in the Rust unit/integration suites; this file only protects the
//! repository contracts that used to live in the Python policy harness.

mod common;

use common::repo_root;
use regex::Regex;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn read_repo(path: &str) -> String {
    let full = repo_root().join(path);
    fs::read_to_string(&full).unwrap_or_else(|error| panic!("read {}: {error}", full.display()))
}

fn files_under(path: &str) -> Vec<PathBuf> {
    fn visit(root: &Path, out: &mut Vec<PathBuf>) {
        for entry in
            fs::read_dir(root).unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        {
            let path = entry
                .unwrap_or_else(|error| panic!("read directory entry: {error}"))
                .path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    let root = repo_root().join(path);
    let mut out = Vec::new();
    visit(&root, &mut out);
    out.sort();
    out
}

#[test]
fn docs_use_canonical_platform_capitalization() {
    let mut violations = Vec::new();
    let macos = Regex::new(r"\b(?:MacOS|Mac OS|OSX|OS X)\b").unwrap();
    let linux = Regex::new(r"(^|[^A-Za-z0-9_./-])linux([^A-Za-z0-9_./-]|$)").unwrap();
    let markdown_ignored = Regex::new(r"`[^`]*`|https?://\S+|\[[^\]]+\]\([^)]+\)").unwrap();
    let mut paths = vec!["README.md", "AGENTS.md", "CONTRIBUTING.md"];
    let docs = files_under("docs");
    paths.extend(
        docs.iter()
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
            .filter_map(|path| path.strip_prefix(repo_root()).ok()?.to_str()),
    );
    for relative in paths {
        let source = read_repo(relative);
        let mut fenced = false;
        for (line_number, raw) in source.lines().enumerate() {
            if raw.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            let line = markdown_ignored.replace_all(raw, "");
            if macos.is_match(&line) {
                violations.push(format!("{relative}:{}: use macOS", line_number + 1));
            }
            if linux.is_match(&line) {
                violations.push(format!("{relative}:{}: use Linux", line_number + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "bad platform capitalization:\n{}",
        violations.join("\n")
    );
}

#[test]
fn egui_is_confined_to_ui_and_main() {
    let patterns = Regex::new(r"use egui\b|use eframe\b|egui::|eframe::").unwrap();
    let line_comments = Regex::new(r"//[^\n]*").unwrap();
    let mut violations = Vec::new();
    for path in files_under("src/rust") {
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let relative = path
            .strip_prefix(repo_root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if relative == "src/rust/main.rs"
            || relative == "src/rust/ui.rs"
            || relative.starts_with("src/rust/ui/")
        {
            continue;
        }
        if relative.starts_with("src/rust/tests/") || relative.ends_with("_tests.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        let stripped = line_comments.replace_all(&source, "");
        if patterns.is_match(&stripped) {
            violations.push(relative);
        }
    }
    assert!(
        violations.is_empty(),
        "egui/eframe leaked outside the UI facade: {violations:?}"
    );
}

#[test]
fn generated_settings_docs_are_present_and_descriptions_are_not_dropped() {
    let docs = read_repo("docs/CONFIGURATION.md");
    let begin = docs
        .find("<!-- BEGIN GENERATED SETTINGS REFERENCE -->")
        .expect("settings begin marker");
    let end = docs
        .find("<!-- END GENERATED SETTINGS REFERENCE -->")
        .expect("settings end marker");
    assert!(begin < end, "settings markers are out of order");
    let schema: Value = serde_json::from_str(&read_repo("shared/config/settings_schema.json"))
        .expect("valid settings schema");
    let settings = schema["settings"].as_array().expect("settings array");
    for setting in settings {
        let description = setting["description"].as_str().unwrap_or_default();
        assert!(
            !description.is_empty(),
            "setting has no description: {setting}"
        );
        assert!(
            docs.contains(description),
            "generated docs omitted description: {description}"
        );
    }
}

#[test]
fn packaging_console_surfaces_are_ascii_safe() {
    let roots = ["packaging"];
    let output =
        Regex::new(r"\b(?:echo|printf|Write-Host|Write-Error|Write-Warning|Write-Output)\b")
            .unwrap();
    let mut violations = Vec::new();
    for root in roots {
        for path in files_under(root) {
            let source = fs::read_to_string(&path).unwrap_or_default();
            for (line_number, line) in source.lines().enumerate() {
                if output.is_match(line) && !line.is_ascii() {
                    violations.push(format!("{}:{}: {line}", path.display(), line_number + 1));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "non-ASCII console output:\n{}",
        violations.join("\n")
    );
}

#[test]
fn production_changes_have_a_test_or_explicit_small_scope() {
    let output = Command::new("git")
        .args(["diff", "--unified=0", "origin/main...HEAD", "--"])
        .current_dir(repo_root())
        .output()
        .expect("git is available");
    if !output.status.success() {
        return;
    }
    let diff = String::from_utf8_lossy(&output.stdout);
    let file_re = Regex::new(r"^\+\+\+ b/(.+)$").unwrap();
    let public_re =
        Regex::new(r"^\+\s*(?:pub\s+)?(?:fn|struct|enum|trait|const|static)\s+([A-Za-z0-9_]+)")
            .unwrap();
    let mut current = None;
    let mut added = 0;
    let mut public = Vec::new();
    let mut offenders = Vec::new();
    let flush =
        |path: Option<String>, added: usize, public: &[String], offenders: &mut Vec<String>| {
            let Some(path) = path else { return };
            if added < 20
                || path.contains("/tests/")
                || path.ends_with("/main.rs")
                || path.starts_with("docs/")
                || path.starts_with(".github/")
            {
                return;
            }
            let matching = if path.ends_with(".rs") {
                path.replace(".rs", "_tests.rs").contains("_tests.rs")
                    || path.starts_with("src/rust/tests/")
            } else {
                false
            };
            if !matching && !public.is_empty() {
                offenders.push(path);
            }
        };
    for line in diff.lines() {
        if let Some(capture) = file_re.captures(line) {
            flush(current.take(), added, &public, &mut offenders);
            current = Some(capture[1].to_owned());
            added = 0;
            public.clear();
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
            if let Some(capture) = public_re.captures(line) {
                public.push(capture[1].to_owned());
            }
        }
    }
    flush(current, added, &public, &mut offenders);
    assert!(
        offenders.is_empty(),
        "production changes need a matching test: {offenders:?}"
    );
}
