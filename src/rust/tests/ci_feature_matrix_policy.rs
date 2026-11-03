//! Event-dependent shape of the `rust-features` CI matrix.
//!
//! Pull requests run every standalone feature cell so feature-gating breaks
//! are caught before merge. Push to main and the release `workflow_call`
//! (whose `github.event_name` is the caller's `push` / `workflow_dispatch`)
//! run only the exact shipped feature set plus clippy. The anchor cell that
//! owns the once-per-run checks must exist on every event.

mod common;

use std::collections::BTreeSet;

const KEPT_OFF_PR: [&str; 2] = ["shipping", "clippy-glow"];
const ANCHOR_PROFILE: &str =
    "ANCHOR_PROFILE: ${{ github.event_name == 'pull_request' && 'base' || 'shipping' }}";

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
fn pull_requests_run_every_standalone_feature_cell() {
    let job = rust_features_job();
    let all: BTreeSet<_> = profile_ids(&job).into_iter().collect();
    for required in [
        "base",
        "hotkeys",
        "audio",
        "hotkeys-injection",
        "local-injection",
    ] {
        assert!(all.contains(required), "missing standalone cell {required}");
    }
    assert_eq!(cells(&job, "pull_request"), all);
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

#[test]
fn once_per_run_checks_follow_an_anchor_cell_present_on_every_event() {
    let job = rust_features_job();
    assert!(job.contains(ANCHOR_PROFILE));
    assert!(cells(&job, "pull_request").contains("base"));
    assert!(cells(&job, "push").contains("shipping"));
    for step in [
        "      - name: Rust format\n",
        "      - name: Rust tests\n",
        "      - name: Rust tests (Windows non-anchor)\n",
        "      - name: Windows release-script fixture\n",
    ] {
        let gate = job
            .split(step)
            .nth(1)
            .and_then(|body| {
                body.lines()
                    .find(|line| line.trim_start().starts_with("if: "))
            })
            .unwrap_or_else(|| panic!("{step:?} step gate"));
        assert!(
            gate.contains("env.ANCHOR_PROFILE"),
            "{step:?} must follow the anchor cell, not a fixed profile: {gate}"
        );
        assert!(!gate.contains("'base'"), "{step:?} pins base: {gate}");
    }
}
