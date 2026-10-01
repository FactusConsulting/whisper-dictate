mod common;

#[test]
fn required_audio_report_job_declares_jq_in_its_own_dependency_step() {
    let workflow =
        std::fs::read_to_string(common::repo_root().join(".github/workflows/test.yml")).unwrap();
    let mut lines = workflow
        .lines()
        .skip_while(|line| *line != "  rust-features:");
    assert_eq!(lines.next(), Some("  rust-features:"));
    let job: Vec<_> = lines
        .take_while(|line| {
            !(line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(':'))
        })
        .collect();
    assert!(job
        .iter()
        .any(|line| line.contains("Audio capture report gate")));
    assert!(
        job.iter().any(|line| {
            line.trim_start().starts_with("packages:")
                && line.split_whitespace().any(|package| package == "jq")
        }),
        "the required audio cell must not rely on an incidental hosted-runner jq"
    );
}
