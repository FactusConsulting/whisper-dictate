//! Compatibility checks run on pull requests without duplicating the
//! shipping test suite.

mod common;

use std::collections::BTreeSet;

const KEPT_OFF_PR: [&str; 2] = ["shipping", "clippy-glow"];

fn rust_features_job() -> String {
    let workflow = std::fs::read_to_string(common::repo_root().join(".github/workflows/test.yml"))
        .expect("read test.yml")
        .replace("\r\n", "\n");
    workflow
        .split("\n  rust-features:\n")
        .nth(1)
        .and_then(|section| section.split("\n  rust:\n").next())
        .expect("rust-features job")
        .to_owned()
}

fn profile_ids(job: &str) -> Vec<String> {
    job.lines()
        .filter_map(|line| line.strip_prefix("          - id: "))
        .map(|id| id.trim().to_owned())
        .collect()
}

/// Profile ids excluded for `event_name`, evaluated from the
/// `exclude: ${{ fromJSON(github.event_name == 'pull_request' && '<pr>' || '<other>') }}` line.
fn excluded_ids(job: &str, event_name: &str) -> BTreeSet<String> {
    let expr = job
        .lines()
        .find_map(|line| line.strip_prefix("        exclude: "))
        .expect("rust-features matrix exclude")
        .strip_prefix("${{ fromJSON(github.event_name == 'pull_request' && '")
        .and_then(|rest| rest.strip_suffix("') }}"))
        .expect("exclude must select its list on github.event_name == 'pull_request'");
    let (on_pr, otherwise) = expr
        .split_once("' || '")
        .expect("exclude must have a pull_request and a non-pull_request list");
    let selected = if event_name == "pull_request" {
        on_pr
    } else {
        otherwise
    };
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(selected).expect("exclude list is JSON");
    entries
        .iter()
        .map(|entry| {
            assert_eq!(
                entry.as_object().map(|object| object.len()),
                Some(1),
                "exclude entries must match on the profile alone, not per OS: {entry}"
            );
            entry["profile"]["id"]
                .as_str()
                .expect("exclude entry names a profile id")
                .to_owned()
        })
        .collect()
}

fn cells(job: &str, event_name: &str) -> BTreeSet<String> {
    let excluded = excluded_ids(job, event_name);
    profile_ids(job)
        .into_iter()
        .filter(|id| !excluded.contains(id))
        .collect()
}

#[test]
fn pull_requests_run_every_cell() {
    let job = rust_features_job();
    let required: BTreeSet<_> = ["shipping", "clippy-glow", "features"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(cells(&job, "pull_request"), required);
}

/// Compile the default and supported partial feature sets with test targets.
#[test]
fn the_features_cell_checks_default_and_each_partial_feature_set() {
    let job = rust_features_job();
    let checks = step(&job, "Rust feature-set checks");
    assert!(checks.contains("matrix.profile.id == 'features'"));
    assert!(checks
        .lines()
        .any(|line| line.contains("cargo check ") && line.ends_with("--all-targets")));
    assert!(
        checks.contains("RUSTFLAGS: -D warnings"),
        "a partial feature set must compile without warnings"
    );
    let sets = checks
        .lines()
        .find_map(|line| line.trim().strip_prefix("for features in "))
        .and_then(|rest| rest.strip_suffix("; do"))
        .expect("feature-set loop");
    let sets: BTreeSet<_> = sets.split_whitespace().collect();
    for required in [
        "rust-hotkeys",
        "audio-capture",
        "rust-hotkeys,rust-injection",
        "whisper-rs-local,audio-capture,rust-injection",
    ] {
        assert!(
            sets.contains(required),
            "feature set {required} is not checked"
        );
    }
    let partial_tests = step(&job, "Rust tests that need a partial feature set");
    assert!(partial_tests.contains("runner.os == 'Linux'"));
    assert!(partial_tests.contains("--lib"));
    assert!(partial_tests.contains("--features rust-hotkeys,rust-injection"));
    assert!(partial_tests.contains(
        "test(=runtime::rust_session_sink_tests::production_sink_refuses_without_real_backends)"
    ));
}

#[test]
fn push_and_release_run_only_the_shipping_and_clippy_cells() {
    let job = rust_features_job();
    let kept: BTreeSet<_> = KEPT_OFF_PR.iter().map(|id| id.to_string()).collect();
    // `push` covers main and the tag-push release; a manual release re-run
    // reaches test.yml through workflow_call with `workflow_dispatch`.
    for event_name in ["push", "workflow_dispatch"] {
        assert_eq!(cells(&job, event_name), kept, "{event_name} cells");
    }
}

fn step<'a>(job: &'a str, name: &str) -> &'a str {
    let header = format!("      - name: {name}\n");
    job.split(header.as_str())
        .nth(1)
        .and_then(|body| body.split("\n      - name: ").next())
        .unwrap_or_else(|| panic!("missing step: {name}"))
}

#[test]
fn default_feature_regressions_are_focused_and_cross_platform() {
    let job = rust_features_job();
    let default_tests = step(&job, "Rust tests that need the default feature set");
    assert!(default_tests.contains("matrix.profile.id == 'features'"));
    assert!(!default_tests.contains("runner.os"));
    assert!(default_tests.contains("--lib"));
    assert!(default_tests.contains("--bin wd"));
    assert!(default_tests.contains("test("));
    assert!(!default_tests.contains("--features"));
}

#[test]
fn shipping_format_and_clippy_report_do_not_repeat_work() {
    let job = rust_features_job();
    let format = step(&job, "Rust format");
    assert!(format.contains("matrix.profile.id == 'shipping'"));
    assert!(format.contains("runner.os == 'Linux'"));
    assert_eq!(job.matches("cargo fmt ").count(), 1);

    let windows_clippy = step(&job, "Rust Clippy");
    assert!(windows_clippy.contains("runner.os == 'Windows'"));
    assert!(windows_clippy.contains("cargo clippy "));
    let linux_clippy = step(&job, "Rust Clippy with a report for Sonar");
    assert!(linux_clippy.contains("runner.os == 'Linux'"));
    assert!(linux_clippy.contains("cargo clippy "));
    assert!(linux_clippy.contains("--message-format=json"));
    assert!(linux_clippy.contains("clippy-report.json"));
    assert_eq!(job.matches("cargo clippy ").count(), 2);
}
