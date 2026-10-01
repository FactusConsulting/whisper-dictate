use super::InjectOutcome;
use anyhow::anyhow;

#[test]
fn success_and_successful_result_have_no_partial_progress() {
    for outcome in [InjectOutcome::ok(), InjectOutcome::from_result(Ok(()))] {
        assert!(outcome.result.is_ok());
        assert!(!outcome.partial);
    }
}

#[test]
fn known_failure_and_result_failure_preserve_error_without_partial_progress() {
    for outcome in [
        InjectOutcome::failed(anyhow!("fixture")),
        InjectOutcome::from_result(Err(anyhow!("fixture"))),
    ] {
        assert_eq!(outcome.result.unwrap_err().to_string(), "fixture");
        assert!(!outcome.partial);
    }
}

#[test]
fn partial_failure_preserves_the_retry_barrier_and_original_error() {
    let outcome = InjectOutcome::partial(anyhow!("fixture after progress"));
    assert!(outcome.partial);
    assert_eq!(
        outcome.result.unwrap_err().to_string(),
        "fixture after progress"
    );
}
