//! Failure and panic containment for cloud checks.

use super::*;

/// Convert one cloud-check attempt into the generic UI task envelope.
///
/// The gRPC transport owns a short-lived Tokio runtime and can panic on a
/// malformed response or a runtime invariant violation. Keep that panic on
/// the result channel instead of letting the worker thread disappear (which
/// used to surface only as the misleading receiver-disconnected message).
pub(super) fn cloud_api_check_result<F>(
    check: &CloudApiCheck,
    operation: String,
    check_fn: F,
) -> BackgroundTaskResult
where
    F: FnOnce(&CloudApiCheck) -> anyhow::Result<CloudApiCheckResult>,
{
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check_fn(check)));
    match outcome {
        Ok(Ok(result)) => BackgroundTaskResult {
            label: "cloud API check",
            command: operation,
            stdout: result.summary(),
            stderr: String::new(),
            success: result.model_available,
            code: None,
            error: None,
        },
        Ok(Err(err)) => BackgroundTaskResult {
            label: "cloud API check",
            command: operation,
            stdout: String::new(),
            stderr: String::new(),
            success: false,
            code: None,
            error: Some(err.to_string()),
        },
        Err(payload) => BackgroundTaskResult {
            label: "cloud API check",
            command: operation,
            stdout: String::new(),
            stderr: String::new(),
            success: false,
            code: None,
            error: Some(format!(
                "cloud API check panicked: {}",
                crate::runtime::in_process::stringify_panic(payload)
            )),
        },
    }
}
