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

/// Classify a *send-stage* `ureq::Error` as a **transport** failure — one
/// where a Python `urllib` retry is safe (cannot double-charge) and may
/// succeed where ureq cannot.
///
/// Every request is issued with `http_status_as_error(false)`, so a non-2xx
/// response never surfaces here as `StatusCode` (it flows through
/// [`check_status`]), and a response body that arrives but fails to parse is
/// handled at its own call site. An error at the send stage therefore means
/// either the request never reached the provider (DNS / connect / TLS
/// handshake against an enterprise CA / registry proxy / socket IO) or the
/// response never completed — the provider was not billed, so Python may
/// retry. The sole exception is a **timeout**: a global timeout can fire
/// *after* the provider received the request, so retrying risks a duplicate
/// charge. Treating only timeouts as terminal keeps the rule robust across
/// ureq versions (it matches just the always-present `Timeout` variant rather
/// than enumerating the feature-gated TLS variants).
pub(crate) fn is_transport_error(err: &ureq::Error) -> bool {
    !matches!(err, ureq::Error::Timeout(_))
}

/// A cloud call failure split by whether a Python fallback retry is safe.
///
/// * [`CloudCallError::Transport`] — the request never reached the provider
///   (see [`is_transport_error`]). The Python path validates TLS through the
///   OS trust store and honours the Windows registry proxy, so it may succeed;
///   because the provider was never billed, a retry cannot double-charge.
/// * [`CloudCallError::Terminal`] — the provider was reached (non-2xx, bad
///   response body) or the outcome is ambiguous (timeout), or the request was
///   rejected before the network (empty key/model). Python would hit the same
///   result or risk a duplicate charge, so the fallback envelope is returned
///   as-is and NOT retried.
#[derive(Debug)]
pub enum CloudCallError {
    Transport(String),
    Terminal(String),
}

impl CloudCallError {
    /// Build from a send-stage `ureq::Error`, classifying transport vs terminal
    /// and prefixing the human-readable `context` (e.g. "Groq chat completion
    /// failed").
    pub(crate) fn from_send(context: &str, err: ureq::Error) -> Self {
        let transport = is_transport_error(&err);
        let message = format!("{context}: {}", http_error(err));
        if transport {
            CloudCallError::Transport(message)
        } else {
            CloudCallError::Terminal(message)
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            CloudCallError::Transport(m) | CloudCallError::Terminal(m) => m,
        }
    }

    pub(crate) fn is_transport(&self) -> bool {
        matches!(self, CloudCallError::Transport(_))
    }
}

impl std::fmt::Display for CloudCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for CloudCallError {}

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

    #[test]
    fn timeout_is_terminal_not_transport() {
        // A global (or any) timeout can fire after the provider received the
        // request, so it must NOT be a Python retry candidate — matching only
        // the always-present `Timeout` variant is what keeps the rule robust.
        assert!(!is_transport_error(&ureq::Error::Timeout(
            ureq::Timeout::Global
        )));
        assert!(!is_transport_error(&ureq::Error::Timeout(
            ureq::Timeout::Connect
        )));
    }

    #[test]
    fn connect_and_dns_failures_are_transport() {
        // The request never reached the provider — safe for the Python path to
        // retry against the OS trust store / registry proxy.
        assert!(is_transport_error(&ureq::Error::HostNotFound));
        assert!(is_transport_error(&ureq::Error::ConnectionFailed));
    }

    #[test]
    fn from_send_classifies_and_prefixes_context() {
        let timeout = CloudCallError::from_send("ctx", ureq::Error::Timeout(ureq::Timeout::Global));
        assert!(!timeout.is_transport());
        assert!(timeout.message().starts_with("ctx: "));

        let connect = CloudCallError::from_send("ctx", ureq::Error::ConnectionFailed);
        assert!(connect.is_transport());
        assert!(connect.message().starts_with("ctx: "));
    }
}
