//! Dependency-Track SBOM upload wiring in `build-ci-images.yml`.
//!
//! The upload must inventory exactly the image the job just pushed (the
//! build step's digest, versioned like the pushed `sha-` tag), receive the
//! API key only through a commit-pinned action, and never fail a publish
//! that already succeeded.

mod common;

use std::fs;

const WORKFLOW: &str = "build-ci-images.yml";
const SBOM_STEP: &str = "- name: Upload SBOM to Dependency-Track";

fn workflow() -> String {
    let path = common::repo_root()
        .join(".github")
        .join("workflows")
        .join(WORKFLOW);
    fs::read_to_string(path).expect("read build-ci-images.yml")
}

/// The step block that starts at `header`, up to the next step.
fn step<'a>(workflow: &'a str, header: &str) -> &'a str {
    let start = workflow
        .find(header)
        .unwrap_or_else(|| panic!("{WORKFLOW} has no step `{header}`"));
    let rest = &workflow[start + header.len()..];
    let end = rest.find("\n      - ").unwrap_or(rest.len());
    &workflow[start..start + header.len() + end]
}

#[test]
fn sbom_upload_follows_the_pushed_build() {
    let workflow = workflow();
    let build = workflow.find("- name: Build and push").expect("build step");
    let sbom = workflow.find(SBOM_STEP).expect("SBOM step");
    assert!(build < sbom, "the SBOM must be uploaded after the push");

    let build_step = step(&workflow, "- name: Build and push");
    assert!(build_step.contains("id: build"), "build step needs an id");
    assert!(build_step.contains("push: true"));
    assert!(
        build_step
            .contains("${{ steps.tags.outputs.image }}:sha-${{ steps.tags.outputs.sha_short }}"),
        "the build must push the sha tag the SBOM version names"
    );

    let sbom_step = step(&workflow, SBOM_STEP);
    assert!(
        sbom_step
            .contains("image: ${{ steps.tags.outputs.image }}@${{ steps.build.outputs.digest }}"),
        "the SBOM must scan the pushed manifest by digest"
    );
    assert!(
        sbom_step.contains("version: sha-${{ steps.tags.outputs.sha_short }}"),
        "the Dependency-Track version must match the pushed sha tag"
    );
}

#[test]
fn sbom_upload_is_pinned_and_never_gates_the_publish() {
    let workflow = workflow();
    let sbom_step = step(&workflow, SBOM_STEP);

    let uses = sbom_step
        .lines()
        .find_map(|line| line.trim().strip_prefix("uses: "))
        .expect("SBOM step uses an action");
    let reference = uses
        .split_whitespace()
        .next()
        .and_then(|action| action.rsplit_once('@'))
        .map(|(_, reference)| reference)
        .expect("action reference");
    assert!(
        reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit()),
        "the action receiving DTRACK_API_KEY must be pinned to a full commit SHA, got `{reference}`"
    );
    assert!(sbom_step.contains("dtrack-api-key: ${{ secrets.DTRACK_API_KEY }}"));

    assert!(
        sbom_step.contains("continue-on-error: true"),
        "a Dependency-Track outage must not fail an already-published image"
    );
    assert!(
        !sbom_step.contains("fail-on-severity"),
        "the build SBOM upload is report-only"
    );
}
