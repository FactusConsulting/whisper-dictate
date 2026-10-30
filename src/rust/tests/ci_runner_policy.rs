//! Runner-selection policy for GitHub Actions workflows.
//!
//! Linux jobs run on the self-hosted `arc-runner-public` ARC runners, whose
//! image lacks tooling the GitHub-hosted images shipped (C toolchain, cmake,
//! libclang, `gh`, `pwsh`, `zip`) and has no Docker daemon. Docker-dependent
//! jobs run on `arc-runner-public-docker`, which fork PRs must never reach.
//! Windows jobs stay GitHub-hosted. These checks pin the runner mapping and
//! that each job bootstraps the tools it uses before using them.

mod common;

use std::fs;

const ARC: &str = "arc-runner-public";
const ARC_DOCKER: &str = "arc-runner-public-docker";
const FORK_GUARD: &str =
    "github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository";
const MATRIX_RUNNER: &str =
    "${{ matrix.os == 'ubuntu-latest' && 'arc-runner-public' || matrix.os }}";

struct Job {
    file: String,
    name: String,
    /// Whether the workflow can be triggered by a pull request.
    pull_request: bool,
    /// Job body with YAML comment lines removed.
    body: String,
}

fn workflow_files() -> Vec<(String, String)> {
    let dir = common::repo_root().join(".github").join("workflows");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("workflows directory")
        .map(|entry| entry.expect("workflow entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yml"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&path).expect("read workflow"))
        })
        .collect();
    files.sort();
    files
}

fn without_comments(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

fn jobs() -> Vec<Job> {
    let mut jobs = Vec::new();
    for (file, text) in workflow_files() {
        let text = without_comments(&text);
        let Some((triggers, section)) = text.split_once("\njobs:\n") else {
            continue;
        };
        let pull_request = triggers.contains("pull_request:");
        let mut current: Option<Job> = None;
        for line in section.lines() {
            let is_job_header = line.starts_with("  ")
                && !line.starts_with("   ")
                && line.trim_end().ends_with(':');
            if is_job_header {
                jobs.extend(current.take());
                current = Some(Job {
                    file: file.clone(),
                    name: line.trim().trim_end_matches(':').to_owned(),
                    pull_request,
                    body: String::new(),
                });
            } else if let Some(job) = current.as_mut() {
                job.body.push_str(line);
                job.body.push('\n');
            }
        }
        jobs.extend(current);
    }
    jobs
}

fn runs_on(job: &Job) -> Option<&str> {
    job.body
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("runs-on: "))
        .map(str::trim)
}

fn uses_arc(job: &Job) -> bool {
    runs_on(job).is_some_and(|runner| runner == ARC || runner == MATRIX_RUNNER)
}

const DOCKER_MARKERS: [&str; 8] = [
    "docker run",
    "docker pull",
    "docker build",
    "uses: docker/",
    "devcontainers/ci",
    "\n    container:",
    "\n    services:",
    "docker/build-push-action",
];

fn needs_docker(job: &Job) -> bool {
    DOCKER_MARKERS
        .iter()
        .any(|marker| job.body.contains(marker))
}

fn before(job: &Job, setup: &str, usage: &str) -> bool {
    match job.body.find(usage) {
        None => true,
        Some(used_at) => job
            .body
            .find(setup)
            .is_some_and(|setup_at| setup_at < used_at),
    }
}

#[test]
fn every_runner_is_arc_windows_or_the_conditional_matrix_mapping() {
    let mut seen = 0;
    for job in jobs() {
        // Reusable-workflow callers have no runner of their own.
        let Some(runner) = runs_on(&job) else {
            assert!(
                job.body.contains("uses: ./.github/workflows/"),
                "{}:{} has neither runs-on nor a local reusable workflow",
                job.file,
                job.name
            );
            continue;
        };
        seen += 1;
        assert!(
            runner == ARC
                || runner == "windows-2025"
                || runner == MATRIX_RUNNER
                || (runner == ARC_DOCKER && needs_docker(&job)),
            "{}:{} uses unexpected runner `{runner}`; Linux jobs belong on `{ARC}` (or \
             `{ARC_DOCKER}` when they need Docker), Windows jobs stay on `windows-2025`",
            job.file,
            job.name
        );
    }
    assert!(seen > 20, "workflow job discovery broke: saw {seen} jobs");
}

#[test]
fn docker_jobs_use_the_docker_runner_behind_a_fork_guard() {
    let mut docker_jobs = Vec::new();
    for job in jobs() {
        let docker = needs_docker(&job);
        let runner = runs_on(&job);
        if uses_arc(&job) {
            assert!(
                !docker,
                "{}:{} needs Docker but `{ARC}` has no Docker daemon; use `{ARC_DOCKER}`",
                job.file, job.name
            );
        }
        if runner == Some(ARC_DOCKER) {
            if job.pull_request {
                assert!(
                    job.body.contains(FORK_GUARD),
                    "{}:{} runs on `{ARC_DOCKER}` but can be reached by fork PRs; add the fork guard",
                    job.file,
                    job.name
                );
            }
            docker_jobs.push(format!("{}:{}", job.file, job.name));
        }
    }
    for expected in [
        "test.yml:lint-workflows",
        "test.yml:integration-ubuntu-2604",
        "build-ci-images.yml:ubuntu-2604",
        "devcontainer.yml:build-and-test",
    ] {
        assert!(
            docker_jobs.iter().any(|found| found == expected),
            "{expected} must run on {ARC_DOCKER}; found {docker_jobs:?}"
        );
    }
}

#[test]
fn matrix_runner_mapping_keeps_windows_hosted_and_os_names_stable() {
    let mut mapped = 0;
    for job in jobs()
        .into_iter()
        .filter(|job| runs_on(job) == Some(MATRIX_RUNNER))
    {
        mapped += 1;
        // The required check names embed `matrix.os`; the runner must be a
        // separate expression, never a rename of the `os` values.
        assert!(
            job.body.contains("os: [ubuntu-latest, windows-2025]"),
            "{}:{} must keep `os: [ubuntu-latest, windows-2025]` so required check names stay stable",
            job.file,
            job.name
        );
    }
    assert!(
        mapped >= 6,
        "expected the mixed matrices to use the mapping, saw {mapped}"
    );
}

#[test]
fn windows_only_jobs_stay_on_github_hosted_windows() {
    let windows: Vec<_> = jobs()
        .into_iter()
        .filter(|job| runs_on(job) == Some("windows-2025"))
        .map(|job| format!("{}:{}", job.file, job.name))
        .collect();
    for expected in [
        "test.yml:legacy-installer",
        "release.yml:install-smoke",
        "windows-installer-build.yml:build-installer",
        "windows-installer-publish.yml:publish",
    ] {
        assert!(
            windows.iter().any(|found| found == expected),
            "{expected} must stay on windows-2025; found {windows:?}"
        );
    }
}

#[test]
fn arc_jobs_install_a_c_toolchain_before_compiling_rust() {
    for job in jobs().into_iter().filter(uses_arc) {
        let first_compile = [
            "cargo build",
            "cargo test",
            "cargo run",
            "cargo clippy",
            "cargo nextest",
            "cargo llvm-cov",
        ]
        .iter()
        .filter_map(|verb| job.body.find(verb))
        .min();
        let Some(compile_at) = first_compile else {
            continue;
        };
        let toolchain_at = job.body.find("build-essential");
        assert!(
            toolchain_at.is_some_and(|at| at < compile_at),
            "{}:{} compiles Rust on the ARC runner without installing build-essential first",
            job.file,
            job.name
        );
    }
}

#[test]
fn arc_jobs_install_gh_pwsh_and_zip_before_using_them() {
    let mut checked = 0;
    for job in jobs().into_iter().filter(uses_arc) {
        let gh_setup = "releases/download/v${GH_VERSION}/gh_";
        for usage in ["gh api ", "gh release "] {
            assert!(
                before(&job, gh_setup, usage),
                "{}:{} calls `{usage}` before installing gh",
                job.file,
                job.name
            );
            checked += usize::from(job.body.contains(usage));
        }
        if runs_on(&job) == Some(ARC) {
            assert!(
                before(&job, "packages.microsoft.com", "pwsh -NoProfile"),
                "{}:{} runs pwsh before installing PowerShell",
                job.file,
                job.name
            );
            assert!(
                before(&job, "            zip \\", "zip -q"),
                "{}:{} runs zip before installing it",
                job.file,
                job.name
            );
            checked += usize::from(job.body.contains("pwsh -NoProfile"))
                + usize::from(job.body.contains("zip -q"));
        }
    }
    assert!(
        checked >= 5,
        "tool-usage discovery broke: saw {checked} usages"
    );
}

#[test]
fn actionlint_knows_the_self_hosted_runner_labels() {
    let config = fs::read_to_string(common::repo_root().join(".github").join("actionlint.yaml"))
        .expect("actionlint config declaring the ARC runner labels");
    for label in [ARC, ARC_DOCKER] {
        assert!(
            config
                .lines()
                .any(|line| line.trim() == format!("- {label}")),
            "`.github/actionlint.yaml` must declare the `{label}` runner label"
        );
    }
}
