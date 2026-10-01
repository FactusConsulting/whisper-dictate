//! Moving the public command types must not bypass their cloud CI trigger.
#[test]
fn groq_integration_tracks_the_extracted_command_families() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let workflow =
        std::fs::read_to_string(root.join(".github/workflows/groq-integration-rust.yml")).unwrap();
    assert!(workflow
        .lines()
        .any(|line| line.trim() == "- 'src/rust/cli/**'"));
}
