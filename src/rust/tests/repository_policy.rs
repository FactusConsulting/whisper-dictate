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

fn tracked_files() -> Vec<String> {
    let output = Command::new("git")
        .args(["ls-files"])
        .current_dir(repo_root())
        .output()
        .expect("git is available");
    assert!(output.status.success(), "git ls-files failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn has_matching_test_file(path: &str, tracked: &[String]) -> bool {
    let Some(stem) = path.strip_suffix(".rs") else {
        return false;
    };
    let basename = path.rsplit('/').next().unwrap_or(path);
    let basename_stem = basename.strip_suffix(".rs").unwrap_or(basename);
    tracked.iter().any(|candidate| {
        candidate == &format!("{stem}_tests.rs")
            || candidate == &format!("src/rust/tests_{basename_stem}.rs")
            || candidate == &format!("src/rust/tests/{basename}")
            || (candidate.ends_with(&format!("/tests/{basename}")) && candidate != path)
    })
}

fn production_diff_is_required(event: Option<&str>) -> bool {
    event == Some("pull_request")
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
fn generated_settings_docs_match_the_schema_exactly() {
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
    let titles = [
        ("core", "Core (the first-time-setup basics)"),
        ("stt-local", "Local speech-to-text (Whisper)"),
        ("stt-cloud", "Cloud speech-to-text (OpenAI-compatible APIs)"),
        ("audio", "Audio capture & voice activity"),
        ("postprocess", "Dictionary & post-processing"),
        ("injection", "Injection, hotkeys & feedback"),
        ("diagnostics", "Diagnostics, history & automation"),
        ("updates", "Update checks"),
    ];
    let known_categories: Vec<&str> = titles.iter().map(|(category, _)| *category).collect();
    for setting in settings {
        let category = setting["category"].as_str().unwrap_or_default();
        assert!(
            known_categories.contains(&category),
            "setting has unknown category {category:?}: {setting}"
        );
    }
    let cell = |value: &str| {
        value
            .replace('|', "\\|")
            .replace(['\r', '\n'], " ")
            .trim()
            .to_owned()
    };
    let mut lines = vec![
        "_Generated from \x60shared/config/settings_schema.json\x60 by \x60scripts/dev/gen-settings-docs.ps1\x60 -- do not edit this block by hand; regenerate with \x60pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/dev/gen-settings-docs.ps1\x60._".to_owned(),
        String::new(),
        "Every runtime setting, grouped by area. **Live** settings apply on the next record start/stop; **Restart** settings (backend, model, device, compute type, hotkey) need the worker restarted. The env var is read at startup; the same name without the \x60VOICEPI_\x60 prefix, lower-cased, is the \x60config.json\x60 key.".to_owned(),
        String::new(),
    ];
    for (category, title) in titles {
        let rows: Vec<&Value> = settings
            .iter()
            .filter(|s| s["category"] == category)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let basic = rows
            .iter()
            .filter(|s| s["advanced"].as_bool() != Some(true))
            .count();
        let suffix = if basic > 0 {
            format!(" -- {basic} basic")
        } else {
            String::new()
        };
        lines.push(format!("### {title}{suffix}"));
        lines.push(String::new());
        lines.push("| Key | Env var | Default | Live/Restart | Description |".to_owned());
        lines.push("|---|---|---|---|---|".to_owned());
        for setting in rows {
            let key = cell(setting["key"].as_str().unwrap_or_default());
            let env = cell(setting["env"].as_str().unwrap_or_default());
            let default = match setting["default"].as_str() {
                Some(value) if !value.is_empty() => format!("\x60{}\x60", cell(value)),
                _ => "_(unset)_".to_owned(),
            };
            let live = if setting["live"].as_bool() == Some(true) {
                "Live"
            } else {
                "Restart"
            };
            let description = cell(setting["description"].as_str().unwrap_or_default());
            assert!(
                !description.is_empty(),
                "setting has no description: {setting}"
            );
            lines.push(format!(
                "| \x60{key}\x60 | \x60{env}\x60 | {default} | {live} | {description} |"
            ));
        }
        lines.push(String::new());
    }
    let expected = lines.join("\n").trim_end().to_owned();
    let actual = docs[begin + "<!-- BEGIN GENERATED SETTINGS REFERENCE -->".len()..end]
        .replace("\r\n", "\n")
        .trim_matches(['\r', '\n'])
        .to_owned();
    assert_eq!(actual, expected, "generated settings block is stale");
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
fn ci_docker_mirror_uses_org_variable_and_falls_back() {
    for path in [
        ".github/workflows/test.yml",
        ".github/workflows/build-ci-images.yml",
    ] {
        let workflow = read_repo(path);
        assert!(
            workflow.contains("NEXUS_DOCKER_MIRROR: ${{ vars.NEXUS_DOCKER_MIRROR }}"),
            "{path} must use the organization variable"
        );
        assert!(
            !workflow.contains("nexusdockerproxy.factus.dk"),
            "{path} must not hardcode the proxy"
        );
        assert!(
            workflow.contains("docker pull"),
            "{path} must exercise mirror/direct pulls"
        );
    }
}

#[test]
fn native_probe_and_version_scripts_retain_regression_guards() {
    let probe = read_repo("scripts/dev/probe-key.ps1");
    assert!(
        probe.contains("key .*not supported"),
        "probe must classify native unsupported-key errors"
    );
    assert!(
        probe.contains("stderrPath"),
        "probe must keep JSON stdout clean"
    );
    let bump = read_repo("scripts/dev/bump-version.ps1");
    assert!(
        bump.contains("\\[package\\]"),
        "Cargo version replacement must be package-scoped"
    );
    let generator = read_repo("scripts/dev/gen-settings-docs.ps1");
    assert!(
        generator.contains("-cne"),
        "generated docs comparison must be case-sensitive"
    );
}

#[test]
fn production_changes_have_a_test_or_explicit_small_scope() {
    if !production_diff_is_required(std::env::var("GITHUB_EVENT_NAME").ok().as_deref()) {
        return;
    }
    let output = Command::new("git")
        .args(["diff", "--unified=0", "origin/main...HEAD", "--"])
        .current_dir(repo_root())
        .output()
        .expect("git is available");
    if !output.status.success() {
        panic!(
            "git diff origin/main...HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let diff = String::from_utf8_lossy(&output.stdout);
    let file_re = Regex::new(r"^\+\+\+ b/(.+)$").unwrap();
    let public_re = Regex::new(
        r"^\+\s*pub(?:\s*\([^)]*\))?\s+(?:async\s+)?(?:unsafe\s+)?(?:fn|struct|enum|trait|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)",
    )
    .unwrap();
    let tracked = tracked_files();
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
            let matching = path.ends_with(".rs") && has_matching_test_file(&path, &tracked);
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

#[test]
fn production_diff_guard_is_scoped_to_pull_requests() {
    assert!(production_diff_is_required(Some("pull_request")));
    assert!(!production_diff_is_required(Some("workflow_call")));
    assert!(!production_diff_is_required(Some("push")));
    assert!(!production_diff_is_required(None));
}
