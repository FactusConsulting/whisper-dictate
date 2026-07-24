//! Shared HTTP helpers for the cloud API surface.
//!
//! Split out of the original `cloud_api.rs` so the per-call sites (check,
//! transcribe, chat) can share the rate-limit handling and stay under the
//! repo's 500-LOC per-file ceiling.

pub(crate) const USER_AGENT: &str =
    "whisper-dictate/0.3 (+https://github.com/FactusConsulting/whisper-dictate)";

/// A shared ureq [`ureq::Agent`] whose TLS validates certificates against the
/// **operating-system trust store** (`rustls-platform-verifier`, via ureq's
/// `platform-verifier` feature) rather than ureq's bundled `webpki-roots`.
///
/// Every cloud call (transcribe / chat / check / postprocess) routes through
/// this agent. The reason is behavioural parity with the Python path being
/// retired: Python's `urllib` validates TLS through the platform store, so a
/// cloud endpoint served behind a private/enterprise CA that is trusted only
/// via the OS store succeeds under Python. A Rust client on bundled roots
/// would fail that TLS handshake and silently degrade (e.g. post-processing
/// falling back to raw text). Using the platform verifier keeps enterprise-CA
/// setups working when the Rust backends become the default.
///
/// The agent is built once and cloned (cheap — `Agent` is `Arc`-backed) so the
/// platform-verifier setup cost is paid a single time per process. Per-request
/// knobs (timeout, `http_status_as_error`) are still applied on the individual
/// request builder; only the root-cert source is agent-scoped here.
pub(crate) fn platform_tls_agent() -> ureq::Agent {
    use std::sync::OnceLock;
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT
        .get_or_init(|| {
            use ureq::tls::{RootCerts, TlsConfig};
            ureq::Agent::config_builder()
                .tls_config(
                    TlsConfig::builder()
                        .root_certs(RootCerts::PlatformVerifier)
                        .build(),
                )
                .build()
                .into()
        })
        .clone()
}

/// Turn a non-2xx response into a descriptive error string, mirroring the
/// previous `ureq::Error::Status` handling. Requests are issued with
/// `http_status_as_error(false)`, so 4xx/5xx responses arrive here as `Ok`
/// and we surface the status code, the `Retry-After` header, and the (best
/// effort) response body — including the dedicated 429 rate-limit message.
/// Returns `Ok(())` for success (2xx) responses.
pub(crate) fn check_status(response: &mut ureq::http::Response<ureq::Body>) -> Result<(), String> {
    let code = response.status().as_u16();
    if (200..300).contains(&code) {
        return Ok(());
    }
    let retry_after = response
        .headers()
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let detail = response.body_mut().read_to_string().unwrap_or_default();
    if code == 429 {
        return Err(rate_limit_message(retry_after.as_deref(), &detail));
    }
    if detail.trim().is_empty() {
        Err(format!("HTTP {code}"))
    } else {
        Err(format!("HTTP {code}: {}", detail.trim()))
    }
}

/// Describe a transport-level `ureq::Error` (timeout, DNS, TLS, IO, …). HTTP
/// status codes are handled separately by [`check_status`], since requests
/// opt out of `http_status_as_error`.
pub(crate) fn http_error(err: ureq::Error) -> String {
    err.to_string()
}

pub(crate) fn rate_limit_message(retry_after: Option<&str>, detail: &str) -> String {
    let mut message = "HTTP 429 Too Many Requests: rate limited by provider".to_owned();
    if let Some(seconds) = retry_after.filter(|value| !value.trim().is_empty()) {
        message.push_str(&format!(" (retry after {}s)", seconds.trim()));
    }
    if !detail.trim().is_empty() {
        message.push_str(&format!(": {}", detail.trim()));
    }
    message
}

pub(crate) fn parse_timeout_ms(raw: &str, default: u64) -> u64 {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value >= 100)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_message_includes_retry_after_and_detail() {
        let message = rate_limit_message(Some(" 12 "), r#"{"error":"rate limit"}"#);

        assert!(message.contains("HTTP 429 Too Many Requests"));
        assert!(message.contains("rate limited"));
        assert!(message.contains("retry after 12s"));
        assert!(message.contains("rate limit"));
    }

    #[test]
    fn rate_limit_message_omits_blank_retry_and_detail() {
        let message = rate_limit_message(None, "");

        assert!(message.contains("HTTP 429"));
        assert!(!message.contains("retry after"));
        assert!(message.ends_with("provider"));
    }

    #[test]
    fn parse_timeout_falls_back_to_default_for_invalid_or_small() {
        assert_eq!(parse_timeout_ms("not a number", 1234), 1234);
        assert_eq!(parse_timeout_ms("50", 1234), 1234);
        assert_eq!(parse_timeout_ms("  300  ", 1234), 300);
    }
}
