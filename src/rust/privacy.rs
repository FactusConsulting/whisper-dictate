use anyhow::{anyhow, Result};

// `"parakeet"` is gone from this set; only the local
// Whisper paths (faster-whisper or the Rust whisper-rs helper) remain.
const LOCAL_BACKENDS: &[&str] = &["whisper", "faster-whisper"];
const LOCAL_PROCESSORS: &[&str] = &["none", "ollama"];

pub fn truthy(value: Option<&str>) -> bool {
    !matches!(
        value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

pub fn assert_local_backend(
    local_only: bool,
    backend: &str,
    feature: &str,
    base_url: Option<&str>,
) -> Result<()> {
    if !local_only {
        return Ok(());
    }
    let normalized = backend.trim().to_ascii_lowercase();
    if LOCAL_BACKENDS.contains(&normalized.as_str()) {
        return Ok(());
    }
    // A self-hosted endpoint on loopback never leaves the machine, so it's
    // compatible with local-only mode.
    if base_url.is_some_and(is_loopback_url) {
        return Ok(());
    }
    Err(anyhow!(
        "VOICEPI_LOCAL_ONLY=1 blocks {feature} backend {backend:?}; choose a local backend, a loopback endpoint, or disable local-only mode."
    ))
}

/// Whether an HTTP(S) or gRPC endpoint targets loopback. Parse the authority
/// with the same URI parser as the HTTP client so query/fragment text cannot
/// impersonate userinfo or a host. Malformed endpoints fail closed.
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(uri) = url.trim().parse::<http::Uri>() else {
        return false;
    };
    if uri
        .scheme_str()
        .is_some_and(|scheme| !matches!(scheme, "http" | "https" | "grpc" | "grpcs"))
    {
        return false;
    }
    let Some(authority) = uri.authority() else {
        return false;
    };
    let host_port = authority.as_str().rsplit('@').next().unwrap_or("");
    let has_port = if host_port.starts_with('[') {
        host_port
            .split_once(']')
            .is_some_and(|(_, suffix)| !suffix.is_empty())
    } else {
        host_port.contains(':')
    };
    if has_port && authority.port_u16().is_none() {
        return false;
    }
    let host = authority.host();
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
        .trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
#[path = "privacy_tests.rs"]
mod endpoint_tests;

pub fn assert_local_processor(local_only: bool, processor: &str) -> Result<()> {
    if !local_only {
        return Ok(());
    }
    let normalized = processor.trim().to_ascii_lowercase();
    if LOCAL_PROCESSORS.contains(&normalized.as_str()) {
        return Ok(());
    }
    Err(anyhow!(
        "VOICEPI_LOCAL_ONLY=1 blocks post-processing provider {processor:?}; choose a local provider or disable local-only mode."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_semantics() {
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("no"),
            Some("off"),
        ] {
            assert!(!truthy(value));
        }
        assert!(truthy(Some("1")));
        assert!(truthy(Some("yes")));
        assert!(truthy(Some("anything")));
    }

    #[test]
    fn local_only_blocks_remote_backend_and_processor() {
        let backend = assert_local_backend(true, "openai:gpt-4o-transcribe", "STT", None)
            .unwrap_err()
            .to_string();
        let processor = assert_local_processor(true, "openai")
            .unwrap_err()
            .to_string();

        assert!(backend.contains("VOICEPI_LOCAL_ONLY=1 blocks STT backend"));
        assert!(processor.contains("blocks post-processing provider"));
    }

    #[test]
    fn local_only_allows_local_backends_and_processors() {
        for backend in LOCAL_BACKENDS {
            assert_local_backend(true, backend, "STT", None).unwrap();
        }
        for processor in LOCAL_PROCESSORS {
            assert_local_processor(true, processor).unwrap();
        }
    }

    #[test]
    fn local_backends_no_longer_include_parakeet() {
        // The Parakeet backend is retired; LOCAL_BACKENDS must not
        // carry it, otherwise local-only mode would silently permit a
        // setting that the rest of the stack rejects.
        assert!(!LOCAL_BACKENDS.contains(&"parakeet"));
        // And with local-only on, `assert_local_backend("parakeet", …)` must
        // now reject the legacy value the same as any other non-local
        // backend. (A user that still has `stt_backend = "parakeet"` in
        // config.json is migrated to whisper at load time — see
        // crate::config::load::migrate_parakeet_backend.)
        assert!(assert_local_backend(true, "parakeet", "STT", None).is_err());
    }

    #[test]
    fn local_only_allows_self_hosted_loopback_endpoint() {
        // A loopback self-hosted STT endpoint is local, so local-only permits it.
        for url in [
            "http://localhost:8000/v1",
            "http://127.0.0.1:8000/v1",
            "https://127.0.0.5/v1",
            "http://[::1]:8000/v1",
            "http://user:pass@localhost:8000/v1",
        ] {
            assert_local_backend(true, "openai", "STT", Some(url)).unwrap();
            assert!(is_loopback_url(url), "{url} should be loopback");
        }
        // A non-loopback endpoint is still blocked.
        assert!(
            assert_local_backend(true, "openai", "STT", Some("https://api.openai.com/v1")).is_err()
        );
        for url in [
            "https://api.openai.com/v1",
            "http://example.com",
            "http://10.0.0.5:8000",
        ] {
            assert!(!is_loopback_url(url), "{url} should not be loopback");
        }
    }
}
